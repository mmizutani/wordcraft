//! The backends the pane offers, and how a conversation gets one.
//!
//! The app says what it has ([`Host`]): its settings folder, where an agent's MCP server reaches
//! the window's room, and the `wordcraft-cli` that server runs. Each backend says whether it is
//! ready, needs a sign-in or an install, or can't run here ([`Offer`]), and is made by its id
//! ([`make`]), the id `assist.backend {id}` names. The scripted [`crate::Fake`] is offered only
//! when the host asks for it (tests, trying the pane without a model).
//!
//! On the desktop the picker lists ChatGPT first ([`crate::chatgpt`]), then, for a host that
//! gives agents a way to the document (`wordcraft-cli`, the room's port), the ACP agents
//! ([`crate::acp::presets`]: Claude Code, Codex, Pi, a custom one), each ready, or with what to
//! install or set up first, and the fake last. The web build offers no backend but the fake
//! ([`DESKTOP`]): the ones that sign in or start agent processes are desktop only, and
//! [`offers_in`] answers for the web on any build.

use std::net::SocketAddr;
use std::path::PathBuf;

use serde::Serialize;

use crate::backend::Backend;
use crate::fake::{FAKE_ID, Fake};

/// What the app has, for the backends it offers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Host {
    /// The settings folder, where sign-in credentials live, sealed with a key in the system's
    /// keychain (mode 0600, like the control key).
    pub settings: Option<PathBuf>,
    /// Where an agent's MCP server reaches the window's room (127.0.0.1 and the control
    /// channel's port, or the room-only port), with the invite's key.
    pub room: Option<SocketAddr>,
    /// The `wordcraft-cli` an agent's MCP server runs (`mcp --connect`).
    pub cli: Option<PathBuf>,
    /// Offer the scripted test backend.
    pub fake: bool,
}

/// Whether a backend can start a conversation now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum Readiness {
    /// It can.
    Ready,
    /// The person signs in first: how.
    SignIn { how: String },
    /// Something has to be installed first: what, and how.
    Install { how: String },
    /// It can't run here: why.
    Unavailable { why: String },
}

/// One backend the pane offers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Offer {
    /// Its id, as `assist.backend {id}` names it.
    pub id: String,
    /// Its name in the picker.
    pub label: String,
    /// Whether it can start a conversation now.
    pub readiness: Readiness,
}

/// Whether this build has the backends that sign in or start agent processes: the desktop app
/// does; the web app doesn't, since a web page can't listen on a loopback port for a sign-in,
/// keep its credentials out of browser storage, or start a program (#26).
pub const DESKTOP: bool = cfg!(not(target_arch = "wasm32"));

/// The backends the host can offer, in the picker's order: ChatGPT, the ACP agents, then the
/// fake.
pub fn offers(host: &Host) -> Vec<Offer> {
    offers_in(host, DESKTOP)
}

/// [`offers`] in a desktop build (`desktop`) or a web one, which offers only the fake (when the
/// host asks for it). A desktop build answers either; a web build has no desktop backend to offer.
pub fn offers_in(host: &Host, desktop: bool) -> Vec<Offer> {
    let mut all = Vec::new();
    #[cfg(not(target_arch = "wasm32"))]
    if desktop {
        all.push(crate::chatgpt::offer(host));
        if host.cli.is_some() || host.room.is_some() {
            all.extend(crate::acp::presets::offers(host));
        }
    }
    #[cfg(target_arch = "wasm32")]
    let _ = desktop;
    if host.fake {
        let info = crate::backend::Backend::info(&Fake::new());
        all.push(Offer { id: info.id, label: info.label, readiness: Readiness::Ready });
    }
    all
}

/// Make the backend `id` names, for a new conversation. An id the host doesn't offer, or one
/// that isn't ready, is an error that says why.
pub fn make(id: &str, host: &Host) -> Result<Box<dyn Backend>, String> {
    let id = id.trim();
    let offered = offers(host);
    let Some(offer) = offered.iter().find(|o| o.id == id) else {
        let ids: Vec<&str> = offered.iter().map(|o| o.id.as_str()).collect();
        let shown: String = id.chars().filter(|c| !c.is_control()).take(64).collect();
        return Err(if ids.is_empty() {
            format!("`{shown}`: no assistant is available here")
        } else {
            format!("`{shown}`: no such assistant here (there are {})", ids.join(", "))
        });
    };
    match &offer.readiness {
        Readiness::Ready => {}
        Readiness::SignIn { how } | Readiness::Install { how } => return Err(format!("{}: {how}", offer.label)),
        Readiness::Unavailable { why } => return Err(format!("{}: {why}", offer.label)),
    }
    match id {
        FAKE_ID => Ok(Box::new(Fake::new())),
        #[cfg(not(target_arch = "wasm32"))]
        crate::chatgpt::ID => match &host.settings {
            Some(settings) => Ok(Box::new(crate::chatgpt::ChatGpt::new(settings))),
            None => Err(format!("{}: WordCraft has no settings folder to keep a sign-in in", offer.label)),
        },
        #[cfg(not(target_arch = "wasm32"))]
        id if id.starts_with("acp:") => crate::acp::presets::make(id, host).map_err(|e| format!("{}: {e}", offer.label)),
        _ => Err(format!("{}: can't be started", offer.label)),
    }
}

#[cfg(test)]
mod tests {
    use super::{Host, Readiness, make, offers};

    fn refused(id: &str, host: &Host) -> String {
        match make(id, host) {
            Ok(_) => panic!("{id} was made"),
            Err(e) => e,
        }
    }

    #[test]
    fn the_fake_is_offered_only_when_asked_for_and_unknown_ids_say_what_there_is() {
        assert!(offers(&Host::default()).iter().all(|o| o.id != "fake"));
        let e = refused("fake", &Host::default());
        assert!(e.contains("no assistant is available") || e.contains("no such assistant"), "{e}");
        let host = Host { fake: true, ..Host::default() };
        let all = offers(&host);
        let fake = all.iter().find(|o| o.id == "fake").unwrap();
        assert_eq!(fake.readiness, Readiness::Ready);
        assert_eq!(make(" fake ", &host).unwrap().info().author, "Test Assistant");
        let e = refused("acp:nope\u{0}\u{7}", &host);
        assert!(e.contains("there are") && e.contains("fake") && !e.contains('\u{0}'), "{e}");
        assert!(refused(&"x".repeat(100_000), &host).len() < 200);
    }

    /// The web build (#26) offers nothing, however much its host has: no ChatGPT, no agent,
    /// and the fake only when asked for. It reads no file (the host's paths needn't exist), so
    /// it holds on every target, the web's included.
    #[test]
    fn the_web_offers_no_backend() {
        let room = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 9));
        let host = Host {
            settings: Some(std::path::PathBuf::from("no-such-settings-folder")),
            room: Some(room),
            cli: Some(std::path::PathBuf::from("no-such-folder").join("wordcraft-cli")),
            fake: false,
        };
        assert!(super::offers_in(&host, false).is_empty());
        assert!(super::offers_in(&Host::default(), false).is_empty());
        let ids: Vec<String> = super::offers_in(&Host { fake: true, ..host.clone() }, false).into_iter().map(|o| o.id).collect();
        assert_eq!(ids, ["fake"]);
        // This build offers what its kind does.
        assert_eq!(offers(&host), super::offers_in(&host, super::DESKTOP));
        assert_eq!(super::DESKTOP, !cfg!(target_arch = "wasm32"));
    }

    /// Desktop only: the web build has no ChatGPT or agent to list.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_picker_lists_chatgpt_then_the_agents_then_the_fake() {
        let room = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 9));
        let host = Host { room: Some(room), cli: Some(std::env::temp_dir().join("wordcraft-cli")), fake: true, ..Host::default() };
        let ids: Vec<String> = offers(&host).into_iter().map(|o| o.id).collect();
        assert_eq!(ids, ["chatgpt", "acp:claude-code", "acp:codex", "acp:pi", "acp:custom", "fake"]);
        // Without a way to the document, no agent.
        let ids: Vec<String> = offers(&Host { fake: true, ..Host::default() }).into_iter().map(|o| o.id).collect();
        assert_eq!(ids, ["chatgpt", "fake"]);
    }
}

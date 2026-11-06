//! The backends the pane offers, and how a conversation gets one.
//!
//! The app says what it has ([`Host`]): its settings folder, where an agent's MCP server reaches
//! the window's room, and the `wordcraft-cli` that server runs. Each backend says whether it is
//! ready, needs a sign-in or an install, or can't run here ([`Offer`]), and is made by its id
//! ([`make`]), the id `assist.backend {id}` names. The scripted [`crate::Fake`] is offered only
//! when the host asks for it (tests, trying the pane without a model).
//!
//! The web build offers no backend: the ones that sign in or start agent processes are desktop
//! only.

use std::net::SocketAddr;
use std::path::PathBuf;

use serde::Serialize;

use crate::backend::Backend;
use crate::fake::{FAKE_ID, Fake};

/// What the app has, for the backends it offers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Host {
    /// The settings folder, where sign-in credentials live (mode 0600, like the control key).
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

/// The backends the host can offer, in the picker's order.
pub fn offers(host: &Host) -> Vec<Offer> {
    let mut all = Vec::new();
    #[cfg(not(target_arch = "wasm32"))]
    all.push(crate::chatgpt::offer(host));
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
}

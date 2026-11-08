//! A relay for the document the window shares (#76): the desktop app opens the shared
//! document's room on a relay it dials out to, so an agent on another machine reaches it at the
//! relay's URL with its invite's key. Nobody runs a tunnel, and nothing listens beyond this
//! computer. The relay (`wordcraft-relay`, `docs/relay.md`) is one the person runs themselves
//! or one someone gives them a token for.
//!
//! `room.relay` sets it up ([`WordApp::relay_method`]); the settings are kept in the settings
//! folder (`relay.json`, readable only by the person), with the room the app opened last, so an
//! agent's URL stays the same from one share to the next. The relay starts when a document is
//! shared and stops with sharing.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::WordApp;

/// The file in the settings folder that keeps the relay's address, token and room.
pub(crate) const SETTINGS_FILE: &str = "relay.json";

/// A relay: its address, the token its operator gave, and the room this computer opened there
/// last (to open the same one again, so agents' URLs keep working).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Settings {
    pub(crate) relay: String,
    pub(crate) token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) room: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) secret: Option<String>,
}

impl Settings {
    // The web app opens no room on a relay.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    fn claim(&self) -> Option<(String, String)> {
        Some((self.room.clone()?, self.secret.clone()?))
    }
}

/// The relay's settings, as the window and a connecting thread both see them.
type Shared = Arc<Mutex<Option<Settings>>>;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Where the shared document's room is on the relay now.
enum Link {
    Connecting,
    /// The relay couldn't be reached: trying again.
    Retrying(String),
    Up(Hosted),
    /// The relay refused (the token): nothing more is tried until the setting changes.
    Failed(String),
}

/// Why the room couldn't be opened, and whether trying again may help.
struct NotOpened {
    why: String,
    retry: bool,
}

/// One connection to the relay, for one shared document.
struct Connection {
    link: Arc<Mutex<Link>>,
    stopped: Arc<AtomicBool>,
}

/// The window's relay: what it is set to, and its connection while a document is shared.
#[derive(Default)]
pub(crate) struct RelayState {
    settings: Shared,
    /// Whether `settings` was read from the settings folder yet.
    loaded: bool,
    connection: Option<Connection>,
    /// The room last closed on the relay, while the relay hears of it: the next one waits for
    /// that, so sharing again at once opens the same room (and URL), not one for a second desktop.
    closing: Option<std::thread::JoinHandle<()>>,
}

/// The settings file in `dir`.
fn settings_path(dir: Option<&Path>) -> Option<PathBuf> {
    dir.map(|d| d.join(SETTINGS_FILE))
}

fn load(path: &Path) -> Option<Settings> {
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str::<Settings>(&text) {
        Ok(s) if !s.relay.is_empty() && !s.token.is_empty() => Some(s),
        _ => {
            // What it holds stays out of the log.
            log::warn!("{} is damaged: ignored", path.display());
            None
        }
    }
}

/// Write `settings` to `path`, readable only by the person (mode 0600 on Unix), or remove the
/// file for `None`.
fn save(path: &Path, settings: Option<&Settings>) -> Result<(), String> {
    let Some(settings) = settings else {
        return match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("{} can't be removed: {e}", path.display())),
            _ => Ok(()),
        };
    };
    let dir = path.parent().ok_or("no folder to keep the relay's settings in")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{} can't be made: {e}", dir.display()))?;
    let bytes = serde_json::to_vec_pretty(settings).map_err(|e| e.to_string())?;
    let temp = dir.join(format!(".{SETTINGS_FILE}.{}.tmp", std::process::id()));
    let written = (|| {
        use std::io::Write;
        let _ = std::fs::remove_file(&temp);
        let mut open = std::fs::OpenOptions::new();
        open.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut open, 0o600);
        let mut f = open.open(&temp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&temp, path)
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("{} can't be written: {e}", path.display()));
    }
    Ok(())
}

/// The MCP client configuration for an agent with `key` reaching the document at `url` (the
/// `mcpServers` form most MCP clients read).
pub(crate) fn mcp_config(url: &str, key: &str) -> Value {
    json!({"mcpServers": {"wordcraft": {"type": "http", "url": url, "headers": {"Authorization": format!("Bearer {key}")}}}})
}

impl WordApp {
    fn relay_settings(&mut self) -> Option<Settings> {
        if !self.relay.loaded {
            self.relay.loaded = true;
            let saved = settings_path(self.services.settings_dir.as_deref()).and_then(|p| load(&p));
            *lock(&self.relay.settings) = saved;
        }
        lock(&self.relay.settings).clone()
    }

    /// `room.relay`: set up, show or turn off the relay agents on other machines reach the
    /// shared document through.
    ///
    /// - `{"relay": address, "token": token}`: use this relay (the token may be left out to keep
    ///   the one saved for the same address). Kept in the settings folder; connects now when a
    ///   document is shared, else when one is.
    /// - `{"off": true}`: stop using a relay, and forget it.
    /// - `{}`: what it is set to.
    ///
    /// The reply ([`WordApp::relay_state`]): the relay's address, how the connection is
    /// (`status`, with `why` when it isn't up), and the URL agents use (`url`) once it is.
    pub(crate) fn relay_method(&mut self, p: &Value) -> Result<Value, String> {
        let saved = self.relay_settings();
        if p.get("off").and_then(Value::as_bool) == Some(true) {
            self.stop_relay();
            if let Some(path) = settings_path(self.services.settings_dir.as_deref()) {
                save(&path, None)?;
            }
            *lock(&self.relay.settings) = None;
            return Ok(self.relay_state());
        }
        let relay = p.get("relay").filter(|v| !v.is_null());
        let token = p.get("token").filter(|v| !v.is_null());
        if relay.is_none() && token.is_none() {
            return Ok(self.relay_state());
        }
        let relay = relay.and_then(Value::as_str).ok_or("`relay`: the relay's address, https://…")?;
        let relay = check_relay(relay)?;
        let token = match token {
            Some(t) => t.as_str().map(str::trim).filter(|t| !t.is_empty()).ok_or("`token`: the token the relay's operator gave")?.to_string(),
            None => match saved.as_ref().filter(|s| s.relay == relay) {
                Some(s) => s.token.clone(),
                None => return Err("missing `token`: the token the relay's operator gave".into()),
            },
        };
        // The same relay keeps its room, so agents' URLs keep working.
        let same = saved.as_ref().filter(|s| s.relay == relay);
        let settings = Settings { relay, token, room: same.and_then(|s| s.room.clone()), secret: same.and_then(|s| s.secret.clone()) };
        if let Some(path) = settings_path(self.services.settings_dir.as_deref()) {
            save(&path, Some(&settings))?;
        }
        *lock(&self.relay.settings) = Some(settings);
        self.stop_relay();
        self.start_relay();
        Ok(self.relay_state())
    }

    /// The relay's address, how the connection is, and the URL agents use once it is up.
    pub(crate) fn relay_state(&self) -> Value {
        let settings = if self.relay.loaded {
            lock(&self.relay.settings).clone()
        } else {
            settings_path(self.services.settings_dir.as_deref()).and_then(|p| load(&p))
        };
        let Some(settings) = settings else { return json!({"relay": null, "status": "off", "url": null}) };
        let Some(c) = &self.relay.connection else {
            let status = if self.share.is_some() { "failed" } else { "waiting" };
            let why =
                if self.share.is_some() { Some("agents on this computer can't reach the shared document, so the relay can't either") } else { None };
            let mut v = json!({"relay": settings.relay, "status": status, "url": null});
            if let Some(why) = why {
                v["why"] = json!(why);
            }
            return v;
        };
        let (status, why, url) = match &*lock(&c.link) {
            Link::Connecting => ("connecting", None, None),
            Link::Retrying(why) => ("retrying", Some(why.clone()), None),
            Link::Failed(why) => ("failed", Some(why.clone()), None),
            Link::Up(h) => {
                let (status, why) = h.status();
                (status, why, Some(h.url()))
            }
        };
        let mut v = json!({"relay": settings.relay, "status": status, "url": url});
        if let Some(why) = why {
            v["why"] = json!(why);
        }
        v
    }

    /// The URL agents use for the shared document, while its room is open on the relay.
    pub(crate) fn relay_url(&self) -> Option<String> {
        let c = self.relay.connection.as_ref()?;
        match &*lock(&c.link) {
            Link::Up(h) => Some(h.url()),
            _ => None,
        }
    }

    /// Open the shared document's room on the relay, when one is set up and a document is shared:
    /// on a thread of its own, since the relay may be slow to answer.
    pub(crate) fn start_relay(&mut self) {
        if self.share.is_none() || self.relay.connection.is_some() {
            return;
        }
        let Some(settings) = self.relay_settings() else { return };
        let Some(room_port) = self.lobby.addr() else {
            log::warn!("relay: the shared document's room port isn't open, so the relay can't reach it");
            return;
        };
        let link = Arc::new(Mutex::new(Link::Connecting));
        let stopped = Arc::new(AtomicBool::new(false));
        let (l, s, shared, ctx) = (link.clone(), stopped.clone(), self.relay.settings.clone(), self.ctx.clone());
        let path = settings_path(self.services.settings_dir.as_deref());
        let closing = self.relay.closing.take();
        let spawned = std::thread::Builder::new().name("relay connect".into()).spawn(move || {
            // The room closed last is closed on the relay first (a few seconds at most).
            if let Some(closing) = closing {
                let _ = closing.join();
            }
            // Until it opens, the relay refuses, or sharing stops: the computer may be offline.
            let mut wait = std::time::Duration::from_secs(2);
            let result = loop {
                match Hosted::start(&settings, &room_port.to_string()) {
                    Err(NotOpened { why, retry: true }) if !s.load(Ordering::SeqCst) => {
                        log::warn!("relay: {why}; trying again in {} s", wait.as_secs());
                        *lock(&l) = Link::Retrying(why);
                        if let Some(ctx) = &ctx {
                            ctx.request_repaint();
                        }
                        for _ in 0..wait.as_millis() / 100 {
                            if s.load(Ordering::SeqCst) {
                                break;
                            }
                            std::thread::sleep(std::time::Duration::from_millis(100));
                        }
                        wait = (wait * 2).min(std::time::Duration::from_secs(60));
                    }
                    other => break other,
                }
            };
            let mut link = lock(&l);
            // Stopped while it connected: close the room again, here, once the window can look
            // at the link again.
            if s.load(Ordering::SeqCst) {
                drop(link);
                drop(result);
                return;
            }
            *link = match result {
                Ok(hosted) => {
                    let (room, secret) = hosted.claim();
                    let mut kept = lock(&shared);
                    if let Some(k) =
                        kept.as_mut().filter(|k| k.relay == settings.relay && (k.room.as_ref(), k.secret.as_ref()) != (Some(&room), Some(&secret)))
                    {
                        k.room = Some(room);
                        k.secret = Some(secret);
                        if let Some(path) = &path
                            && let Err(e) = save(path, Some(k))
                        {
                            log::warn!("relay: {e}");
                        }
                    }
                    Link::Up(hosted)
                }
                Err(NotOpened { why, .. }) => {
                    log::warn!("relay: {why}");
                    Link::Failed(why)
                }
            };
            drop(link);
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        });
        if let Err(e) = spawned {
            *lock(&link) = Link::Failed(format!("no thread for the relay: {e}"));
        }
        self.relay.connection = Some(Connection { link, stopped });
    }

    /// Close the shared document's room on the relay: agents' requests are refused from now on.
    /// Telling the relay happens on a thread of its own.
    pub(crate) fn stop_relay(&mut self) {
        let Some(c) = self.relay.connection.take() else { return };
        let up = {
            let mut link = lock(&c.link);
            c.stopped.store(true, Ordering::SeqCst);
            std::mem::replace(&mut *link, Link::Failed("stopped".into()))
        };
        if let Link::Up(hosted) = up {
            // The room refuses agents at once; the relay hears it on a thread of its own.
            self.relay.closing = hosted.stop_soon();
        }
    }
}

/// Whether `relay` may be a relay's address, without a trailing `/`.
fn check_relay(relay: &str) -> Result<String, String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        wordcraft_relay::relay_url(relay)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = relay;
        Err("the web app can't open a room on a relay: share from the desktop app".into())
    }
}

/// The shared document's room open on a relay, its agents' requests answered through the room
/// port at the address given.
#[cfg(not(target_arch = "wasm32"))]
struct Hosted(Arc<wordcraft_relay::host::Hosting>);

#[cfg(not(target_arch = "wasm32"))]
impl Hosted {
    fn start(settings: &Settings, room_port: &str) -> Result<Hosted, NotOpened> {
        use wordcraft_mcp::http::{Endpoint, Options};
        use wordcraft_relay::host::Status;
        // Behind the relay, requests come with the relay's name in `Host`; the relay passes
        // `Origin` on, so a web page is still refused.
        let endpoint = Arc::new(Endpoint::connecting(Options { hosts: None, ..Options::default() }, room_port));
        let handler: wordcraft_http::Handler = Arc::new(move |req| endpoint.handle(req));
        wordcraft_relay::host::Hosting::open(&settings.relay, &settings.token, settings.claim(), handler).map(|h| Hosted(Arc::new(h))).map_err(|s| {
            match s {
                Status::Retrying(why) => NotOpened { why, retry: true },
                Status::Refused(why) => NotOpened { why, retry: false },
                other => NotOpened { why: format!("{other:?}"), retry: false },
            }
        })
    }

    fn url(&self) -> String {
        self.0.url().to_string()
    }

    fn claim(&self) -> (String, String) {
        self.0.claim()
    }

    /// `connected`, `retrying` or `refused`, with why when it isn't connected.
    fn status(&self) -> (&'static str, Option<String>) {
        use wordcraft_relay::host::Status;
        match self.0.status() {
            Status::Connected => ("connected", None),
            Status::Retrying(why) => ("retrying", Some(why)),
            Status::Refused(why) => ("refused", Some(why)),
            Status::Stopped => ("stopped", None),
        }
    }

    /// Close the room on the relay, on a thread of its own when one starts.
    fn stop_soon(self) -> Option<std::thread::JoinHandle<()>> {
        let hosting = self.0;
        let spawned = std::thread::Builder::new().name("relay stop".into()).spawn({
            let hosting = hosting.clone();
            move || hosting.stop()
        });
        match spawned {
            Ok(handle) => Some(handle),
            Err(_) => {
                hosting.stop();
                None
            }
        }
    }
}

/// The web app opens no room on a relay ([`check_relay`] says why).
#[cfg(target_arch = "wasm32")]
struct Hosted;

#[cfg(target_arch = "wasm32")]
impl Hosted {
    fn start(_: &Settings, _: &str) -> Result<Hosted, NotOpened> {
        Err(NotOpened { why: "the web app can't open a room on a relay".into(), retry: false })
    }
    fn url(&self) -> String {
        String::new()
    }
    fn claim(&self) -> (String, String) {
        (String::new(), String::new())
    }
    fn status(&self) -> (&'static str, Option<String>) {
        ("stopped", None)
    }
    fn stop_soon(self) -> Option<std::thread::JoinHandle<()>> {
        None
    }
}

#[cfg(test)]
mod tests;

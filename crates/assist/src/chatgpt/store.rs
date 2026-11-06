//! What Sign in with ChatGPT keeps in the settings folder:
//!
//! - [`CLIENT_FILE`]: this installation's host id (`ext_agent_host_id`, made before the first
//!   sign-in and kept for good) and the client id the first sign-in was issued. Kept when the
//!   person signs out.
//! - [`CREDENTIALS_FILE`]: the signed-in account and its tokens. Deleted when they sign out.
//!
//! Both are written atomically (a temporary file in the same folder, then a rename) with mode
//! 0600 on Unix, read with a size cap, and a damaged file reads as missing. The tokens never
//! reach the log: [`Credentials`]'s `Debug` leaves them out.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use super::http::{now, random};

/// The credentials file, in the settings folder.
pub const CREDENTIALS_FILE: &str = "chatgpt-credentials.json";

/// The host id and issued client id, in the settings folder.
pub const CLIENT_FILE: &str = "chatgpt-client.json";

/// The lock file that keeps two WordCraft processes from refreshing at once.
const LOCK_FILE: &str = "chatgpt-credentials.lock";

/// Most bytes either file may have; a bigger one reads as damaged.
const MAX_FILE: u64 = 256 << 10;

/// The credentials' format.
const VERSION: u32 = 1;

/// This installation's host id and the client id it was issued.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct Client {
    /// `urn:uuid:<v4>`: sent with every sign-in, never changed.
    pub(crate) host_id: String,
    /// The `oaiapp_…` client id the first sign-in was issued, once it was.
    pub(crate) client_id: Option<String>,
    /// The model the person chose, if they chose one.
    pub(crate) model: Option<String>,
}

/// The signed-in account and its tokens.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Credentials {
    pub(crate) version: u32,
    pub(crate) client_id: String,
    pub(crate) host_id: String,
    pub(crate) issuer: String,
    pub(crate) subject: String,
    #[serde(default)]
    pub(crate) email: String,
    #[serde(default)]
    pub(crate) name: Option<String>,
    /// The last id token: the next sign-in's `id_token_hint` (it may have expired).
    #[serde(default)]
    pub(crate) id_token: String,
    /// Empty once a refresh failed for good: the person signs in again.
    #[serde(default)]
    pub(crate) access_token: String,
    /// Empty once a refresh failed for good.
    #[serde(default)]
    pub(crate) refresh_token: String,
    #[serde(default)]
    pub(crate) token_type: String,
    /// When the access token expires, in seconds since 1970.
    #[serde(default)]
    pub(crate) expires_at: u64,
    /// The scopes the token endpoint granted.
    #[serde(default)]
    pub(crate) scopes: Vec<String>,
    #[serde(default)]
    pub(crate) saved_at: u64,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Tokens stay out of logs.
        f.debug_struct("Credentials")
            .field("email", &self.email)
            .field("expires_at", &self.expires_at)
            .field("scopes", &self.scopes)
            .finish_non_exhaustive()
    }
}

impl Credentials {
    /// Whether plan usage was granted (`chatgpt.tokens.use.direct`).
    pub(crate) fn plan_usage(&self) -> bool {
        self.scopes.iter().any(|s| s == super::PLAN_SCOPE)
    }

    /// Whether it can still refresh: the person is signed in.
    pub(crate) fn signed_in(&self) -> bool {
        !self.refresh_token.is_empty()
    }

    pub(crate) fn account(&self) -> Account {
        Account { email: self.email.clone(), name: self.name.clone(), plan_usage: self.plan_usage() }
    }
}

/// Who is signed in to ChatGPT, as Options › Account shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    /// The account's email address.
    pub email: String,
    /// The account's name, when the id token had one.
    pub name: Option<String>,
    /// Whether the person allowed WordCraft to use their ChatGPT plan (Plus and Pro); without
    /// it the assistant can't answer.
    pub plan_usage: bool,
}

/// Who is signed in, from the credentials file in `settings` (no network). None when nobody
/// is, or the file is missing or damaged.
pub fn account(settings: &Path) -> Option<Account> {
    read_credentials(settings).filter(Credentials::signed_in).map(|c| c.account())
}

/// The credentials, if the file holds them.
pub(crate) fn read_credentials(settings: &Path) -> Option<Credentials> {
    let c: Credentials = read_json(&settings.join(CREDENTIALS_FILE))?;
    (c.version == VERSION && !c.client_id.is_empty()).then_some(c)
}

/// Keep `c` (atomically, mode 0600).
pub(crate) fn write_credentials(settings: &Path, c: &Credentials) -> Result<(), String> {
    let mut c = c.clone();
    c.version = VERSION;
    c.saved_at = now();
    let bytes = serde_json::to_vec_pretty(&c).map_err(|e| e.to_string())?;
    write_private(&settings.join(CREDENTIALS_FILE), &bytes)
}

/// Delete the credentials file. Its being gone already is fine.
pub(crate) fn delete_credentials(settings: &Path) -> Result<(), String> {
    match fs::remove_file(settings.join(CREDENTIALS_FILE)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("the sign-in couldn't be deleted: {e}")),
    }
}

/// This installation's client record, made (with a new host id) and kept if there is none: the
/// host id exists before the first sign-in.
pub(crate) fn client(settings: &Path) -> Result<Client, String> {
    if let Some(c) = read_json::<Client>(&settings.join(CLIENT_FILE)).filter(|c| valid_host_id(&c.host_id)) {
        return Ok(c);
    }
    let c = Client { host_id: new_host_id()?, client_id: None, model: None };
    save_client(settings, &c)?;
    Ok(c)
}

/// Keep the client record.
pub(crate) fn save_client(settings: &Path, c: &Client) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(c).map_err(|e| e.to_string())?;
    write_private(&settings.join(CLIENT_FILE), &bytes)
}

/// `urn:uuid:` and a random (version 4) UUID.
fn new_host_id() -> Result<String, String> {
    let mut b = random(16)?;
    if let Some(x) = b.get_mut(6) {
        *x = (*x & 0x0f) | 0x40;
    }
    if let Some(x) = b.get_mut(8) {
        *x = (*x & 0x3f) | 0x80;
    }
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    let part = |r: std::ops::Range<usize>| hex.get(r).unwrap_or_default().to_string();
    Ok(format!("urn:uuid:{}-{}-{}-{}-{}", part(0..8), part(8..12), part(12..16), part(16..20), part(20..32)))
}

fn valid_host_id(id: &str) -> bool {
    id.strip_prefix("urn:uuid:").is_some_and(|u| u.len() == 36 && u.chars().all(|c| c.is_ascii_hexdigit() || c == '-'))
}

/// A JSON file of at most [`MAX_FILE`] bytes, or None when it's missing, too big or damaged.
fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    let file = fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE.saturating_add(1)).read_to_end(&mut bytes).ok()?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_FILE {
        log::warn!("{} is too big: ignored", path.display());
        return None;
    }
    match serde_json::from_slice(&bytes) {
        Ok(v) => Some(v),
        Err(_) => {
            // What it holds stays out of the log.
            log::warn!("{} is damaged: ignored", path.display());
            None
        }
    }
}

/// Write `bytes` to `path` atomically: a new file next to it (mode 0600 on Unix), synced, then
/// renamed over it.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let dir = path.parent().ok_or("no folder to keep it in")?;
    fs::create_dir_all(dir).map_err(|e| format!("{} can't be made: {e}", dir.display()))?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let suffix: String = random(6)?.iter().map(|b| format!("{b:02x}")).collect();
    let temp = dir.join(format!(".{name}.{suffix}.tmp"));
    let written = (|| {
        let mut open = fs::OpenOptions::new();
        open.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut open, 0o600);
        let mut f = open.open(&temp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&temp, path)
    })();
    if let Err(e) = written {
        let _ = fs::remove_file(&temp);
        return Err(format!("{} can't be written: {e}", path.display()));
    }
    Ok(())
}

/// The process's own lock on refreshing (one refresh at a time across its windows).
static REFRESHING: Mutex<()> = Mutex::new(());

/// Longest [`Lock::take`] waits for another process's refresh.
const LOCK_WAIT: Duration = Duration::from_secs(20);

/// A lock file older than this was left by a process that stopped: it's taken over.
const STALE_LOCK: Duration = Duration::from_secs(60);

/// Held while the credentials are refreshed or signed out: no other refresh in this process,
/// nor in another WordCraft process with the same settings folder (a lock file).
pub(crate) struct Lock {
    _process: MutexGuard<'static, ()>,
    file: Option<PathBuf>,
}

impl Lock {
    /// Wait for the lock. The file lock is skipped (with a warning) when the folder can't hold
    /// one, and taken over when it's stale or the wait is over: a refresh never waits forever.
    pub(crate) fn take(settings: &Path) -> Lock {
        let process = REFRESHING.lock().unwrap_or_else(PoisonError::into_inner);
        let path = settings.join(LOCK_FILE);
        let until = Instant::now() + LOCK_WAIT;
        let _ = fs::create_dir_all(settings);
        let mut took_over = false;
        loop {
            match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(_) => return Lock { _process: process, file: Some(path) },
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let age = fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| SystemTime::now().duration_since(t).ok());
                    if age.is_some_and(|a| a > STALE_LOCK) || Instant::now() >= until {
                        if took_over {
                            // It came back, or can't be removed: go on without it rather than wait.
                            log::warn!("ChatGPT sign-in: the lock file {} stays; going on without it", path.display());
                            return Lock { _process: process, file: None };
                        }
                        log::warn!("ChatGPT sign-in: taking over the lock file {}", path.display());
                        let _ = fs::remove_file(&path);
                        took_over = true;
                        continue;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => {
                    log::warn!("ChatGPT sign-in: no lock file ({e}); refreshing without one");
                    return Lock { _process: process, file: None };
                }
            }
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        if let Some(path) = &self.file {
            let _ = fs::remove_file(path);
        }
    }
}

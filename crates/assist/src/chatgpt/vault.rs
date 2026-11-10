//! The ChatGPT sign-in at rest.
//!
//! With a keychain (macOS Keychain, Windows Credential Manager, the Secret Service over D-Bus on
//! Linux and the BSDs), the credentials are sealed with AES-256-GCM in [`CREDENTIALS_FILE`] in
//! the settings folder, and the key, 256 random bits, is the only thing in the keychain: Windows
//! keeps at most 2,560 bytes in one credential, and the tokens can be more. Each settings folder
//! has a key of its own ([`key_name`]), so two profiles, or two tests, never share one.
//!
//! With none (the system has none, it fails, or the program never asked for it:
//! [`use_system_keychain`]), the sign-in is kept in this process's memory while it runs and
//! nothing readable is written to disk: the person signs in again the next time.
//!
//! A [`PLAINTEXT_FILE`] an older WordCraft left is sealed (or taken into memory) and deleted the
//! first time the folder is opened. Signing out deletes both files, the key and the memory copy.
//!
//! The keychain may take its time (an unlock prompt waits for the person; a bus may not answer),
//! so the window's thread never asks it: [`peek`] answers from what is already known and leaves
//! the rest to a thread of its own, which [`opening`] reports. Everything else here runs on the
//! threads that sign in, refresh and sign out.
//!
//! A sealed file that can't be read whole, is too big, isn't this format's first version, or
//! doesn't open with the key reads as signed out, as a damaged file always has
//! (`store::read_json`), and stays where it is until the next sign-in replaces it.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};

use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::http::{b64url, from_b64url, now};
use super::store::{CREDENTIALS_FILE, Credentials, Kept, Lock, MAX_FILE, PLAINTEXT_FILE, VERSION, read_capped, read_json, write_private};

/// The keychain service the keys are kept under; the account names the settings folder.
const SERVICE: &str = "WordCraft";

/// What the sealed file says it is.
const FORMAT: &str = "wordcraft.chatgpt-credentials";

/// The sealed file's format version.
const FORMAT_VERSION: u32 = 1;

/// How it is sealed.
const ALGORITHM: &str = "AES-256-GCM";

/// The data the ciphertext is bound to: it opens only as ChatGPT credentials in this format's
/// first version, so nothing else sealed with the key, now or in a later format, passes for them.
const AAD: &[u8] = b"wordcraft.chatgpt-credentials/1/AES-256-GCM";

/// Most bytes the sealed file may have: credentials as big as the JSON files may be
/// ([`MAX_FILE`]), with the tag, in base64url, and the envelope around them. Sealing makes the
/// credentials a third bigger, so a sign-in whose tokens are each as long as the token endpoint
/// may send (`auth::MAX_TOKEN`) seals past `MAX_FILE`.
pub(super) const MAX_SEALED: u64 = (MAX_FILE + 16).div_ceil(3) * 4 + 1024;

/// An AES-256 key.
type Key = [u8; 32];

/// The sealed file: what it is, how it was sealed, and the sealed credentials (base64url).
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    format: String,
    version: u32,
    algorithm: String,
    nonce: String,
    ciphertext: String,
}

/// Where the keys are kept: the system's keychain, or a stand-in in tests. Every call may take a
/// while, so none is made on the window's thread.
pub(crate) trait Keychain: Send + Sync {
    /// The secret kept for `account`; None when there is none.
    fn get(&self, account: &str) -> Result<Option<Vec<u8>>, String>;
    /// Keep `secret` for `account`, in place of any there.
    fn set(&self, account: &str, secret: &[u8]) -> Result<(), String>;
    /// Delete `account`'s secret; its being gone already is fine.
    fn delete(&self, account: &str) -> Result<(), String>;
}

/// The system's keychain, through `keyring`.
struct System;

impl Keychain for System {
    fn get(&self, account: &str) -> Result<Option<Vec<u8>>, String> {
        match entry(account)?.get_secret() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn set(&self, account: &str, secret: &[u8]) -> Result<(), String> {
        entry(account)?.set_secret(secret).map_err(|e| e.to_string())
    }

    fn delete(&self, account: &str) -> Result<(), String> {
        match entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

fn entry(account: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, account).map_err(|e| e.to_string())
}

/// A keychain in this process's memory, for tests: never the system's.
#[derive(Default)]
pub(crate) struct Fake {
    pub(crate) entries: Mutex<HashMap<String, Vec<u8>>>,
    /// Every call fails, as a locked or missing keychain does. A test may lock it mid-session.
    pub(crate) broken: AtomicBool,
}

impl Keychain for Fake {
    fn get(&self, account: &str) -> Result<Option<Vec<u8>>, String> {
        if self.broken.load(Ordering::Relaxed) {
            return Err("no keychain".into());
        }
        Ok(guard(&self.entries).get(account).cloned())
    }

    fn set(&self, account: &str, secret: &[u8]) -> Result<(), String> {
        if self.broken.load(Ordering::Relaxed) {
            return Err("no keychain".into());
        }
        guard(&self.entries).insert(account.to_string(), secret.to_vec());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<(), String> {
        if self.broken.load(Ordering::Relaxed) {
            return Err("no keychain".into());
        }
        guard(&self.entries).remove(account);
        Ok(())
    }
}

/// What this process knows of one settings folder's sign-in.
#[derive(Default)]
enum Mode {
    /// The keychain hasn't been asked yet.
    #[default]
    Unknown,
    /// The keychain answers: the sign-in is sealed in [`CREDENTIALS_FILE`] with this key, once
    /// there is one.
    Keychain(Option<Key>),
    /// No keychain: the sign-in, if any, is here only.
    Memory(Option<Box<Credentials>>),
}

#[derive(Default)]
struct State {
    mode: Mode,
    /// A thread is opening the folder for [`peek`].
    asking: bool,
    /// The hash of the sealed file last found damaged: [`peek`] doesn't send a thread to the
    /// keychain about it again.
    damaged: Option<Vec<u8>>,
}

/// One settings folder's sign-in, shared by every thread of this process. The state's lock is
/// never held while the keychain or the disk is asked.
struct Vault {
    keychain: Option<Arc<dyn Keychain>>,
    state: Mutex<State>,
}

/// Whether the program asked for the system's keychain ([`use_system_keychain`]).
static SYSTEM: AtomicBool = AtomicBool::new(false);

/// Each settings folder's vault, by the path it is named with.
static VAULTS: LazyLock<Mutex<HashMap<PathBuf, Arc<Vault>>>> = LazyLock::new(Mutex::default);

/// Keep ChatGPT sign-ins in the system's keychain from now on. The desktop app calls this
/// first thing. Without it (tests, other tools) a sign-in is kept for the session only, so
/// nothing but the app itself ever reaches the person's keychain.
pub fn use_system_keychain() {
    SYSTEM.store(true, Ordering::Relaxed);
}

/// For tests: `settings`' key goes to a keychain in this process's memory, never the system's,
/// shared by every folder as the system's is (each folder's key under its own name);
/// `working: false` makes it fail, as a locked or missing keychain does. Call it before the
/// folder is used: what this process knew of it is forgotten.
#[doc(hidden)]
pub fn fake_keychain(settings: &Path, working: bool) {
    static WORKING: LazyLock<Arc<Fake>> = LazyLock::new(Arc::default);
    static BROKEN: LazyLock<Arc<Fake>> = LazyLock::new(|| Arc::new(Fake { broken: AtomicBool::new(true), ..Fake::default() }));
    register(settings, Some(if working { WORKING.clone() } else { BROKEN.clone() }));
}

/// `settings`' key goes to `keychain` from now on (None: there is none); what this process knew
/// of the folder is forgotten, as when WordCraft opens again.
pub(crate) fn register(settings: &Path, keychain: Option<Arc<dyn Keychain>>) {
    guard(&VAULTS).insert(settings.to_path_buf(), Arc::new(Vault { keychain, state: Mutex::default() }));
}

/// Whether a thread is reading `settings`' sign-in from the keychain: [`super::account`] says
/// nobody is signed in until it is done.
pub fn opening(settings: &Path) -> bool {
    guard(&vault(settings).state).asking
}

fn vault(settings: &Path) -> Arc<Vault> {
    let mut all = guard(&VAULTS);
    let vault = all.entry(settings.to_path_buf()).or_insert_with(|| {
        let keychain: Option<Arc<dyn Keychain>> = if SYSTEM.load(Ordering::Relaxed) { Some(Arc::new(System)) } else { None };
        Arc::new(Vault { keychain, state: Mutex::default() })
    });
    vault.clone()
}

fn guard<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The credentials and where they are kept, as far as this process knows without asking the
/// keychain: for the window's thread. While the folder hasn't been opened, an older file waits to
/// be sealed, or the key doesn't open the file (another WordCraft signed in again with a new
/// one), it answers None and a thread of its own finds out.
pub(crate) fn peek(settings: &Path) -> Option<(Credentials, Kept)> {
    let vault = vault(settings);
    let (known, key, damaged) = {
        let state = guard(&vault.state);
        match &state.mode {
            Mode::Memory(c) => return c.as_deref().cloned().map(|c| (c, Kept::Session)),
            Mode::Keychain(key) => (true, *key, state.damaged.clone()),
            Mode::Unknown => (false, None, None),
        }
    };
    let older = settings.join(PLAINTEXT_FILE).exists();
    let sealed = read_capped(&settings.join(CREDENTIALS_FILE), MAX_SEALED);
    // Nothing to read and nothing to seal: no reason to ask the keychain.
    if !older && sealed.is_none() {
        return None;
    }
    let mut found = None;
    if known && let Some(bytes) = &sealed {
        found = key.and_then(|k| unseal(&k, bytes)).map(|c| (c, Kept::Keychain));
        if found.is_none() && !older && damaged == Some(fingerprint(bytes)) {
            return None;
        }
    }
    if found.is_none() || older {
        look_later(settings, &vault);
    }
    found
}

/// Open the folder and read it on a thread of its own, unless one already is: [`peek`] knows
/// the answer afterwards.
fn look_later(settings: &Path, vault: &Arc<Vault>) {
    {
        let mut state = guard(&vault.state);
        if state.asking {
            return;
        }
        state.asking = true;
    }
    let done = Asking(vault.clone());
    let (settings, vault) = (settings.to_path_buf(), vault.clone());
    let spawned = std::thread::Builder::new().name("chatgpt keychain".into()).spawn(move || {
        let _done = done;
        open(&settings, &vault);
        let _ = read(&settings, &vault);
    });
    if let Err(e) = spawned {
        log::warn!("ChatGPT sign-in: the keychain can't be asked here: {e}");
    }
}

/// Clears [`State::asking`] when the thread that set it ends, however it ends (or never starts).
struct Asking(Arc<Vault>);

impl Drop for Asking {
    fn drop(&mut self) {
        guard(&self.0.state).asking = false;
    }
}

/// The credentials, if the folder (or, with no keychain, this process) holds them. It may ask
/// the keychain: never on the window's thread.
pub(crate) fn read_credentials(settings: &Path) -> Option<Credentials> {
    let vault = vault(settings);
    open(settings, &vault);
    read(settings, &vault)
}

/// Keep `c`: sealed with the keychain's key, or in memory when there is no keychain. It may ask
/// the keychain: never on the window's thread.
pub(crate) fn write_credentials(settings: &Path, c: &Credentials) -> Result<Kept, String> {
    let vault = vault(settings);
    open(settings, &vault);
    let mut c = c.clone();
    c.version = VERSION;
    c.saved_at = now();
    keep(settings, &vault, c)
}

/// Sign out here: the sealed file, an older plaintext one, the key and the memory copy all go.
/// Their being gone already is fine. A key the keychain won't delete stays, with a warning: it
/// opens nothing any more.
pub(crate) fn delete_credentials(settings: &Path) -> Result<(), String> {
    let vault = vault(settings);
    {
        let mut state = guard(&vault.state);
        state.mode = match &state.mode {
            Mode::Memory(_) => Mode::Memory(None),
            Mode::Keychain(_) => Mode::Keychain(None),
            Mode::Unknown => Mode::Unknown,
        };
        state.damaged = None;
    }
    let mut failed = Ok(());
    for name in [CREDENTIALS_FILE, PLAINTEXT_FILE] {
        match fs::remove_file(settings.join(name)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => failed = Err(format!("the sign-in couldn't be deleted: {e}")),
        }
    }
    if let Some(keychain) = &vault.keychain
        && let Err(e) = keychain.delete(&key_name(settings))
    {
        log::warn!("ChatGPT sign-out: the sign-in's key stays in the keychain ({e}); it opens nothing now");
    }
    failed
}

/// Ask the keychain about this folder, once, then seal (or take into memory) and delete a
/// plaintext file an older WordCraft left. The asking needs no lock, so a keychain that waits
/// for the person holds up no refresh or sign-out; the moving is done under the credentials'
/// lock, so no other WordCraft with this folder makes a second key or moves the file at the same
/// time. A thread that holds the lock already (a refresh, a sign-in being kept) goes on under it.
fn open(settings: &Path, vault: &Vault) {
    if matches!(guard(&vault.state).mode, Mode::Unknown) {
        let mode = ask(settings, vault);
        let mut state = guard(&vault.state);
        if matches!(state.mode, Mode::Unknown) {
            state.mode = mode;
        }
    }
    if !settings.join(PLAINTEXT_FILE).exists() {
        return;
    }
    let lock = if Lock::held_here() { Ok(None) } else { Lock::take(settings).map(Some) };
    match lock {
        Ok(_held) => migrate(settings, vault),
        // Another WordCraft is busy with the sign-in: an older file waits for the next time.
        Err(e) => log::warn!("ChatGPT sign-in: {e}"),
    }
}

/// What the keychain says of this folder: its key, none yet, or no keychain (memory only).
fn ask(settings: &Path, vault: &Vault) -> Mode {
    let Some(keychain) = &vault.keychain else {
        log::info!("ChatGPT sign-in: no keychain here; a sign-in is kept for this session only");
        return Mode::Memory(None);
    };
    match get_key(keychain.as_ref(), settings) {
        Ok(key) => Mode::Keychain(key),
        Err(e) => {
            log::warn!("ChatGPT sign-in: the keychain can't be used ({e}); a sign-in is kept for this session only");
            Mode::Memory(None)
        }
    }
}

/// The plaintext file an older WordCraft kept, sealed or taken into memory, then deleted. Its
/// tokens never stay on disk readable: one that can't be read is deleted too, and one that
/// can't be sealed is kept in memory.
fn migrate(settings: &Path, vault: &Vault) {
    let older = settings.join(PLAINTEXT_FILE);
    if !older.exists() {
        return;
    }
    if let Some(c) = read_json::<Credentials>(&older).filter(Credentials::valid)
        && let Err(e) = keep(settings, vault, c.clone())
    {
        log::warn!("ChatGPT sign-in: the older sign-in couldn't be sealed ({e}); it is kept for this session only");
        guard(&vault.state).mode = Mode::Memory(Some(Box::new(c)));
    }
    match fs::remove_file(&older) {
        Ok(()) => log::info!("ChatGPT sign-in: the older, unsealed sign-in file is deleted"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => log::warn!("ChatGPT sign-in: {} couldn't be deleted: {e}", older.display()),
    }
}

/// Read the sign-in once the folder is open. A key that doesn't open the file (or none) is asked
/// for again, once: another WordCraft may have signed in again since, with a new key.
fn read(settings: &Path, vault: &Vault) -> Option<Credentials> {
    let key = match &guard(&vault.state).mode {
        Mode::Memory(c) => return c.as_deref().cloned(),
        Mode::Keychain(key) => *key,
        Mode::Unknown => return None,
    };
    let path = settings.join(CREDENTIALS_FILE);
    let bytes = read_capped(&path, MAX_SEALED)?;
    if let Some(c) = key.and_then(|k| unseal(&k, &bytes)) {
        return Some(c);
    }
    let damaged = fingerprint(&bytes);
    if guard(&vault.state).damaged.as_ref() == Some(&damaged) {
        return None;
    }
    let keychain = vault.keychain.as_ref()?;
    match get_key(keychain.as_ref(), settings) {
        Ok(fresh) => {
            let mut state = guard(&vault.state);
            if matches!(state.mode, Mode::Keychain(_)) {
                state.mode = Mode::Keychain(fresh);
            }
            drop(state);
            if let Some(c) = fresh.filter(|f| Some(*f) != key).and_then(|k| unseal(&k, &bytes)) {
                return Some(c);
            }
        }
        Err(e) => {
            log::warn!("ChatGPT sign-in: the keychain can't be used ({e}); a sign-in is kept for this session only");
            guard(&vault.state).mode = Mode::Memory(None);
            return None;
        }
    }
    // What it holds stays out of the log.
    log::warn!("{} is damaged: ignored", path.display());
    guard(&vault.state).damaged = Some(damaged);
    None
}

/// Keep `c`: sealed in the settings folder with the keychain's key (made the first time), or in
/// memory when there is no keychain, it fails, or the sealed file would be past [`MAX_SEALED`].
/// In memory, a sealed file from before is deleted: `c` replaces it, and left on disk it would
/// come back the next time the keychain answers, with a refresh token ChatGPT may have rotated
/// since.
fn keep(settings: &Path, vault: &Vault, c: Credentials) -> Result<Kept, String> {
    let path = settings.join(CREDENTIALS_FILE);
    if let Some(key) = sealing_key(settings, vault) {
        let sealed = seal(&key, &c)?;
        // A file the next read refuses would lose the sign-in as soon as it is written.
        if u64::try_from(sealed.len()).unwrap_or(u64::MAX) <= MAX_SEALED {
            write_private(&path, &sealed)?;
            return Ok(Kept::Keychain);
        }
        log::warn!("ChatGPT sign-in: too big to seal in a file WordCraft reads back; it is kept for this session only");
    }
    guard(&vault.state).mode = Mode::Memory(Some(Box::new(c)));
    match fs::remove_file(&path) {
        Ok(()) => log::info!("ChatGPT sign-in: the older sealed sign-in is deleted; this one is kept for this session only"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => log::warn!("ChatGPT sign-in: the older sealed sign-in, {}, couldn't be deleted: {e}", path.display()),
    }
    Ok(Kept::Session)
}

/// The key to seal with: the keychain's, or a new one put there. Asked for each time, since
/// another WordCraft may have made one or signed out and deleted it. None when there is no
/// keychain, or it fails: from then on the sign-in is kept in memory.
fn sealing_key(settings: &Path, vault: &Vault) -> Option<Key> {
    if matches!(guard(&vault.state).mode, Mode::Memory(_)) {
        return None;
    }
    let keychain = vault.keychain.as_ref()?;
    let found = get_key(keychain.as_ref(), settings).and_then(|key| match key {
        Some(key) => Ok(key),
        None => {
            let key = new_key()?;
            keychain.set(&key_name(settings), &key)?;
            Ok(key)
        }
    });
    match found {
        Ok(key) => {
            guard(&vault.state).mode = Mode::Keychain(Some(key));
            Some(key)
        }
        Err(e) => {
            log::warn!("ChatGPT sign-in: the keychain can't keep the sign-in's key ({e}); it is kept for this session only");
            None
        }
    }
}

/// The keychain account of `settings`' key: a short hash of the folder's full path, so each
/// settings folder (each profile, each test's) has its own.
fn key_name(settings: &Path) -> String {
    let path = fs::canonicalize(settings).or_else(|_| std::path::absolute(settings)).unwrap_or_else(|_| settings.to_path_buf());
    let hash = Sha256::digest(path.as_os_str().as_encoded_bytes());
    let short: String = hash.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("chatgpt-credentials-key:{short}")
}

/// `settings`' key in `keychain`. A secret there that isn't a key reads as none, so the next
/// sign-in replaces it.
fn get_key(keychain: &dyn Keychain, settings: &Path) -> Result<Option<Key>, String> {
    let found = keychain.get(&key_name(settings))?;
    Ok(found.and_then(|secret| {
        let key = Key::try_from(secret.as_slice()).ok();
        if key.is_none() {
            log::warn!("ChatGPT sign-in: the sign-in's key in the keychain is damaged; the next sign-in replaces it");
        }
        key
    }))
}

fn new_key() -> Result<Key, String> {
    let mut key = [0u8; 32];
    SystemRandom::new().fill(&mut key).map_err(|_| "no random numbers for the sign-in's key".to_string())?;
    Ok(key)
}

fn fingerprint(bytes: &[u8]) -> Vec<u8> {
    Sha256::digest(bytes).to_vec()
}

fn cipher(key: &Key) -> Option<LessSafeKey> {
    UnboundKey::new(&AES_256_GCM, key).ok().map(LessSafeKey::new)
}

/// `c` sealed with `key` and a fresh random nonce, in its envelope.
fn seal(key: &Key, c: &Credentials) -> Result<Vec<u8>, String> {
    let mut data = serde_json::to_vec(c).map_err(|e| e.to_string())?;
    let mut nonce = [0u8; NONCE_LEN];
    SystemRandom::new().fill(&mut nonce).map_err(|_| "no random numbers to seal the sign-in with".to_string())?;
    let cipher = cipher(key).ok_or("the sign-in's key can't be used")?;
    cipher
        .seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::from(AAD), &mut data)
        .map_err(|_| "the sign-in couldn't be sealed".to_string())?;
    let envelope =
        Envelope { format: FORMAT.into(), version: FORMAT_VERSION, algorithm: ALGORITHM.into(), nonce: b64url(&nonce), ciphertext: b64url(&data) };
    serde_json::to_vec_pretty(&envelope).map_err(|e| e.to_string())
}

/// The credentials `bytes` seal with `key`; None when they are anything else.
fn unseal(key: &Key, bytes: &[u8]) -> Option<Credentials> {
    let envelope: Envelope = serde_json::from_slice(bytes).ok()?;
    if envelope.format != FORMAT || envelope.version != FORMAT_VERSION || envelope.algorithm != ALGORITHM {
        return None;
    }
    let nonce = <[u8; NONCE_LEN]>::try_from(from_b64url(&envelope.nonce)?.as_slice()).ok()?;
    let mut data = from_b64url(&envelope.ciphertext)?;
    let plain = cipher(key)?.open_in_place(Nonce::assume_unique_for_key(nonce), Aad::from(AAD), &mut data).ok()?;
    serde_json::from_slice::<Credentials>(plain).ok().filter(Credentials::valid)
}

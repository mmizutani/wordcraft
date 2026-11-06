//! The sign-in itself: an authorization code with PKCE, caught on a loopback port, exchanged for
//! tokens that are kept in the settings folder and refreshed before they expire.
//!
//! A sign-in attempt ([`SignIn`]) listens on `127.0.0.1` (port [`DEFAULT_PORT`], or any free
//! one) before it gives the window the address to open, with a fresh `state`, `nonce` and PKCE
//! verifier. Its thread answers the requests on the port, each read on a thread of its own (an
//! idle connection doesn't hold back the browser's): a `state` that isn't this attempt's is
//! refused (the attempt goes on waiting, however many there are), an `error` ends it, and a
//! `code` is exchanged at the token endpoint with the verifier (no client secret). The first
//! sign-in registers WordCraft (`dynamic_agent_client`) and keeps the client id it is issued,
//! before the exchange; later ones reuse it, until ChatGPT no longer knows it
//! (`invalid_client`): then it is forgotten, and the next sign-in registers again. The id
//! token's claims are checked (issuer, audience, expiry, nonce); its signature is not, as
//! OpenID Connect Core §3.1.3.7 allows for an id token taken straight from the token endpoint
//! over TLS.
//!
//! Plan usage is granted only with the [`PLAN_SCOPE`] scope: a sign-in without it is kept, and
//! the account says so ([`Account::plan_usage`]); signing in again with `consent` asks again.
//!
//! Refreshes ([`access_token`]), sign-outs and the saving of a new sign-in take turns, in the
//! process and (with the system's lock on a file) between WordCraft processes, because each
//! refresh token works once. A refresh keeps its tokens only in place of the ones it was sent
//! with: tokens that come back after a sign-out are revoked, not saved.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::Endpoints;
use super::http::{self, Reply, b64url, from_b64url, now, random, redact, same_secret};
use super::store::{self, Account, Client, Credentials, Lock};

/// The loopback port a sign-in listens on when it's free (else any free port).
pub const DEFAULT_PORT: u16 = 1455;

/// The callback's path on the loopback port.
pub const CALLBACK_PATH: &str = "/auth/callback";

/// The scopes a sign-in asks for.
pub const SCOPES: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";

/// The scope that lets WordCraft use the person's ChatGPT plan.
pub const PLAN_SCOPE: &str = "chatgpt.tokens.use.direct";

/// The client id a first sign-in registers with; the one it is issued replaces it for good.
const REGISTRATION_CLIENT: &str = "dynamic_agent_client";

/// The name the first sign-in registers WordCraft under.
const AGENT_NAME: &str = "WordCraft";

/// How long a sign-in waits for the person to finish in the browser.
pub const SIGN_IN_WAIT: Duration = Duration::from_secs(10 * 60);

/// Most bytes of a callback request's line and headers.
const MAX_REQUEST_HEAD: usize = 8 << 10;

/// Most requests on the sign-in port read at once; the rest wait their turn.
const MAX_CALLBACK_CONNECTIONS: usize = 16;

/// Longest a callback request takes to arrive, and its answer to go out.
const CALLBACK_IO: Duration = Duration::from_secs(5);

/// An access token with less than this left is refreshed first.
const REFRESH_MARGIN: u64 = 5 * 60;

/// Clock difference allowed when checking an id token's expiry.
const SKEW: u64 = 60;

/// Most bytes of a token taken from the token endpoint.
const MAX_TOKEN: usize = 64 << 10;

/// The refresh errors that mean the person signs in again.
const SIGN_IN_AGAIN: [&str; 6] =
    ["invalid_grant", "invalid_refresh_token", "token_expired", "refresh_token_expired", "refresh_token_invalidated", "refresh_token_reused"];

/// Why a sign-in, a refresh or a sign-out didn't work, as the person reads it. No message holds
/// a token.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    /// The person (or the window) stopped the sign-in.
    #[error("the sign-in was cancelled")]
    Cancelled,
    /// Nobody finished signing in in time.
    #[error("the sign-in wasn't finished in the browser in time ({0} min): start it again")]
    TimedOut(u64),
    /// The person didn't allow WordCraft to use their plan.
    #[error("ChatGPT plan usage wasn't allowed, so WordCraft can't use your plan. Sign in again and allow it (ChatGPT Plus or Pro)")]
    Denied,
    /// Nobody is signed in.
    #[error("sign in with ChatGPT first (Options › Account)")]
    NotSignedIn,
    /// The sign-in ended (its refresh token was refused): the person signs in again.
    #[error("your ChatGPT sign-in has ended: sign in again (Options › Account)")]
    SignInAgain,
    /// The servers couldn't be reached.
    #[error("ChatGPT can't be reached: {0}")]
    Network(String),
    /// The sign-in couldn't be kept or read.
    #[error("{0}")]
    Storage(String),
    /// Anything else, said plainly.
    #[error("{0}")]
    Failed(String),
}

/// How a sign-in starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignInOptions {
    /// Where the sign-in happens (OpenAI's, or fake servers in tests).
    pub endpoints: Endpoints,
    /// The port to listen on when it's free; 0 for any.
    pub port: u16,
    /// How long to wait for the person: any length (one too long to count never ends on its
    /// own; cancelling or dropping the attempt ends it).
    pub wait: Duration,
    /// Ask the person again whether WordCraft may use their plan (after they declined).
    pub consent: bool,
}

impl Default for SignInOptions {
    fn default() -> Self {
        SignInOptions { endpoints: Endpoints::default(), port: DEFAULT_PORT, wait: SIGN_IN_WAIT, consent: false }
    }
}

/// One sign-in attempt: open [`SignIn::url`] in the browser, then [`SignIn::poll`] each frame
/// (or [`SignIn::wait`]) until it gives the account or why it failed. Dropping it cancels it.
pub struct SignIn {
    url: String,
    redirect: String,
    done: Receiver<Result<Account, AuthError>>,
    stop: Arc<AtomicBool>,
    result: Option<Result<Account, AuthError>>,
}

impl std::fmt::Debug for SignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The address may hold the last id token (`id_token_hint`): it stays out of logs.
        f.debug_struct("SignIn").field("redirect", &self.redirect).finish_non_exhaustive()
    }
}

impl SignIn {
    /// Start signing in with the settings folder `settings`: listen for the browser, and make
    /// the address the browser opens. The installation's host id is made and kept first.
    pub fn start(settings: &Path, options: SignInOptions) -> Result<SignIn, AuthError> {
        options.endpoints.check().map_err(AuthError::Failed)?;
        let client = store::client(settings).map_err(AuthError::Storage)?;
        let listener = listen(options.port)?;
        let port = listener.local_addr().map_err(|e| AuthError::Failed(format!("the sign-in port has no address: {e}")))?.port();
        let redirect = format!("http://127.0.0.1:{port}{CALLBACK_PATH}");
        let secret = |n| random(n).map(|b| b64url(&b)).map_err(AuthError::Failed);
        let (state, nonce, verifier) = (secret(32)?, secret(32)?, secret(48)?);
        let challenge = b64url(Sha256::digest(verifier.as_bytes()).as_slice());
        let saved = store::read_credentials(settings);
        let url =
            authorize_url(&options, &client, saved.as_ref(), &Params { redirect: &redirect, state: &state, nonce: &nonce, challenge: &challenge });
        let pending = Pending {
            settings: settings.to_path_buf(),
            endpoints: options.endpoints.clone(),
            redirect: redirect.clone(),
            state,
            nonce,
            verifier,
            client,
        };
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, done) = channel();
        // A wait too long to count has no deadline.
        let until = Instant::now().checked_add(options.wait);
        let minutes = u64::try_from(options.wait.as_millis().div_ceil(60_000)).unwrap_or(u64::MAX).max(1);
        let stopping = stop.clone();
        std::thread::Builder::new()
            .name("chatgpt sign-in".into())
            .spawn(move || {
                let result = wait_for_callback(&listener, &pending, &stopping, until, minutes);
                if let Err(e) = &result {
                    log::info!("ChatGPT sign-in: {e}");
                }
                let _ = tx.send(result);
            })
            .map_err(|e| AuthError::Failed(format!("the sign-in can't start here: {e}")))?;
        log::info!("ChatGPT sign-in: waiting on {redirect}");
        Ok(SignIn { url, redirect, done, stop, result: None })
    }

    /// The address to open in the browser. It may hold the last sign-in's id token as a hint:
    /// never log it.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The loopback address the browser comes back to.
    pub fn redirect_uri(&self) -> &str {
        &self.redirect
    }

    /// The result, once there is one (it stays there): never waits.
    pub fn poll(&mut self) -> Option<&Result<Account, AuthError>> {
        if self.result.is_none() {
            match self.done.try_recv() {
                Ok(r) => self.result = Some(r),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.result = Some(Err(AuthError::Failed("the sign-in stopped unexpectedly".into()))),
            }
        }
        self.result.as_ref()
    }

    /// Wait at most `timeout` for the result.
    pub fn wait(&mut self, timeout: Duration) -> Option<&Result<Account, AuthError>> {
        if self.result.is_none() {
            match self.done.recv_timeout(timeout) {
                Ok(r) => self.result = Some(r),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => self.result = Some(Err(AuthError::Failed("the sign-in stopped unexpectedly".into()))),
            }
        }
        self.result.as_ref()
    }

    /// Stop waiting for the browser: the attempt ends with [`AuthError::Cancelled`] (unless it
    /// already ended, or is finishing the browser's answer).
    pub fn cancel(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for SignIn {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// The loopback listener: `port` when it's free, else any free port.
fn listen(port: u16) -> Result<TcpListener, AuthError> {
    let bound = TcpListener::bind(("127.0.0.1", port)).or_else(|e| if port == 0 { Err(e) } else { TcpListener::bind(("127.0.0.1", 0)) });
    let listener = bound.map_err(|e| AuthError::Failed(format!("no port on this computer is free for the sign-in: {e}")))?;
    listener.set_nonblocking(true).map_err(|e| AuthError::Failed(format!("the sign-in port can't be watched: {e}")))?;
    Ok(listener)
}

/// What one attempt's address carries.
struct Params<'a> {
    redirect: &'a str,
    state: &'a str,
    nonce: &'a str,
    challenge: &'a str,
}

/// The authorization address: a first sign-in registers WordCraft (`dynamic_agent_client`,
/// `agent_name_hint`); later ones use the issued client id, with the last id token as a hint.
fn authorize_url(options: &SignInOptions, client: &Client, saved: Option<&Credentials>, p: &Params<'_>) -> String {
    let issued = client.client_id.as_deref().filter(|id| valid_client_id(id));
    let mut pairs: Vec<(&str, &str)> = vec![("client_id", issued.unwrap_or(REGISTRATION_CLIENT))];
    if issued.is_none() {
        pairs.push(("agent_name_hint", AGENT_NAME));
    }
    pairs.push(("ext_agent_host_id", &client.host_id));
    if let Some(saved) = saved.filter(|s| issued == Some(s.client_id.as_str())) {
        if !saved.id_token.is_empty() {
            pairs.push(("id_token_hint", &saved.id_token));
        }
        if !saved.email.is_empty() {
            pairs.push(("login_hint", &saved.email));
        }
    }
    pairs.extend([
        ("response_type", "code"),
        ("redirect_uri", p.redirect),
        ("scope", SCOPES),
        ("resource", options.endpoints.resource.as_str()),
        ("state", p.state),
        ("nonce", p.nonce),
        ("code_challenge_method", "S256"),
        ("code_challenge", p.challenge),
    ]);
    if options.consent && issued.is_some() {
        pairs.push(("prompt", "consent"));
    }
    format!("{}?{}", options.endpoints.authorize, http::query(&pairs))
}

/// Whether `id` can be an issued client id: `oaiapp_` and up to 200 more letters, digits, `-`
/// or `_`.
pub(super) fn valid_client_id(id: &str) -> bool {
    id.strip_prefix("oaiapp_")
        .is_some_and(|rest| !rest.is_empty() && rest.len() <= 200 && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
}

/// What the attempt's thread needs to finish it.
struct Pending {
    settings: PathBuf,
    endpoints: Endpoints,
    redirect: String,
    state: String,
    nonce: String,
    verifier: String,
    client: Client,
}

/// Answer requests on the port, each read on a thread of its own (at most
/// [`MAX_CALLBACK_CONNECTIONS`] at once), until the browser's answer with this attempt's state
/// finishes the sign-in, the attempt is cancelled, or its time (`until`; None: none) is up.
fn wait_for_callback(listener: &TcpListener, p: &Pending, stop: &AtomicBool, until: Option<Instant>, minutes: u64) -> Result<Account, AuthError> {
    let (tx, callbacks) = channel();
    let busy = Arc::new(AtomicUsize::new(0));
    let state: Arc<str> = Arc::from(p.state.as_str());
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err(AuthError::Cancelled);
        }
        if until.is_some_and(|until| Instant::now() >= until) {
            return Err(AuthError::TimedOut(minutes));
        }
        if let Ok(callback) = callbacks.try_recv() {
            return finish_callback(p, callback);
        }
        if busy.load(Ordering::Relaxed) >= MAX_CALLBACK_CONNECTIONS {
            // The rest wait in the port's queue.
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }
        match listener.accept() {
            Ok((stream, _)) => read_on_its_own(stream, &state, &busy, &tx),
            // Nothing yet (or a connection that went away before it was taken): look again.
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

/// The browser's answer with this attempt's state, for the attempt's thread to finish.
struct Callback {
    stream: TcpStream,
    params: Vec<(String, String)>,
}

/// A request being read on the sign-in port, counted in `busy` while it lives.
struct Reading(Arc<AtomicUsize>);

impl Reading {
    fn start(busy: &Arc<AtomicUsize>) -> Reading {
        busy.fetch_add(1, Ordering::Relaxed);
        Reading(busy.clone())
    }
}

impl Drop for Reading {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Read the request on `stream` on a thread of its own: the browser's answer with `state` goes
/// to `tx`; anything else is answered there.
fn read_on_its_own(stream: TcpStream, state: &Arc<str>, busy: &Arc<AtomicUsize>, tx: &Sender<Callback>) {
    let reading = Reading::start(busy);
    let (state, tx) = (state.clone(), tx.clone());
    let spawned = std::thread::Builder::new().name("chatgpt sign-in request".into()).spawn(move || {
        let _reading = reading;
        if let Some(callback) = read_callback(stream, &state) {
            let _ = tx.send(callback);
        }
    });
    if let Err(e) = spawned {
        // The connection closes unanswered (with the thread's work): the browser can try again.
        log::warn!("ChatGPT sign-in: a request on the port can't be read here: {e}");
    }
}

/// The browser's answer with `state` on `stream`, or None when the request was something else
/// (it is answered: not found, not allowed, or not for this attempt).
fn read_callback(mut stream: TcpStream, state: &str) -> Option<Callback> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(CALLBACK_IO));
    let _ = stream.set_write_timeout(Some(CALLBACK_IO));
    let Some(head) = read_head(&mut stream) else {
        respond(&mut stream, 400, "WordCraft couldn't read this request.");
        return None;
    };
    let mut parts = head.lines().next().unwrap_or_default().split(' ');
    let (method, target) = (parts.next().unwrap_or_default(), parts.next().unwrap_or_default());
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != CALLBACK_PATH {
        respond(&mut stream, 404, "There's nothing here.");
        return None;
    }
    if method != "GET" {
        respond(&mut stream, 405, "WordCraft only takes the browser's sign-in answer here.");
        return None;
    }
    let params = http::parse_query(query);
    let sent = params.iter().find(|(k, _)| k == "state").map(|(_, v)| v.as_str());
    if !sent.is_some_and(|s| same_secret(s, state)) {
        respond(&mut stream, 400, "This sign-in answer isn't for the sign-in WordCraft is waiting for. Start signing in again from WordCraft.");
        return None;
    }
    Some(Callback { stream, params })
}

/// Finish the sign-in from the browser's answer, and tell the browser how it went.
fn finish_callback(p: &Pending, mut callback: Callback) -> Result<Account, AuthError> {
    let params = &callback.params;
    let param = |name: &str| params.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());
    let result = finish(p, param("error"), param("error_description"), param("code"), param("client_id"));
    match &result {
        Ok(_) => respond(&mut callback.stream, 200, "You're signed in. You can close this tab and go back to WordCraft."),
        Err(e) => respond(&mut callback.stream, 400, &format!("The sign-in didn't work: {e}. You can close this tab and go back to WordCraft.")),
    }
    result
}

/// The request line and headers of a request, at most [`MAX_REQUEST_HEAD`] bytes: None when
/// they don't arrive whole within [`CALLBACK_IO`].
fn read_head(stream: &mut TcpStream) -> Option<String> {
    let mut head = Vec::new();
    let mut chunk = [0u8; 1024];
    let until = Instant::now().checked_add(CALLBACK_IO);
    while head.len() < MAX_REQUEST_HEAD {
        // Each read waits only for the time left.
        let left = until.map_or(CALLBACK_IO, |until| until.saturating_duration_since(Instant::now()));
        if left.is_zero() || stream.set_read_timeout(Some(left)).is_err() {
            break;
        }
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => head.extend_from_slice(chunk.get(..n).unwrap_or_default()),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
        if head.windows(4).any(|w| w == b"\r\n\r\n") || head.windows(2).any(|w| w == b"\n\n") {
            break;
        }
    }
    // The request line is all that's needed: it must have come whole.
    let text = String::from_utf8_lossy(&head).into_owned();
    text.contains('\n').then_some(text)
}

/// Send a small page with `status` saying `message`, and close.
fn respond(stream: &mut TcpStream, status: u16, message: &str) {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Bad Request",
    };
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>WordCraft</title></head><body style=\"font-family:sans-serif;margin:3em\"><p>{}</p></body></html>",
        escape_html(message)
    );
    let reply = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(reply.as_bytes());
    let _ = stream.flush();
}

fn escape_html(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '&' => "&amp;".to_string(),
            '<' => "&lt;".to_string(),
            '>' => "&gt;".to_string(),
            '"' => "&quot;".to_string(),
            c => c.to_string(),
        })
        .collect()
}

/// Finish the sign-in from the callback's parameters (its `state` already checked).
fn finish(p: &Pending, error: Option<&str>, description: Option<&str>, code: Option<&str>, issued: Option<&str>) -> Result<Account, AuthError> {
    if let Some(error) = error {
        if error == "access_denied" {
            return Err(AuthError::Denied);
        }
        let why = description.map_or_else(|| error.to_string(), |d| format!("{error}: {d}"));
        return Err(AuthError::Failed(format!("ChatGPT refused the sign-in ({})", redact(&why, &[]))));
    }
    let code = code.filter(|c| !c.is_empty() && c.len() <= 4096).ok_or_else(|| AuthError::Failed("the browser's answer has no code".into()))?;
    let client_id = client_id(p, issued)?;
    exchange(p, &client_id, code)
}

/// The client id to exchange the code with: the one this installation was issued, or, on the
/// first sign-in, the one the callback brings (kept at once, before the exchange).
fn client_id(p: &Pending, issued: Option<&str>) -> Result<String, AuthError> {
    match p.client.client_id.as_deref().filter(|id| valid_client_id(id)) {
        Some(saved) => match issued {
            Some(other) if other != saved => {
                Err(AuthError::Failed("the browser's answer names another WordCraft registration: start signing in again".into()))
            }
            _ => Ok(saved.to_string()),
        },
        None => {
            let id = issued
                .filter(|id| valid_client_id(id))
                .ok_or_else(|| AuthError::Failed("WordCraft's registration with ChatGPT didn't finish: start signing in again".into()))?;
            let _lock = Lock::take(&p.settings).map_err(AuthError::Storage)?;
            let current = store::client(&p.settings).map_err(AuthError::Storage)?;
            let client = Client { client_id: Some(id.to_string()), ..current };
            store::save_client(&p.settings, &client).map_err(AuthError::Storage)?;
            Ok(id.to_string())
        }
    }
}

/// Forget the issued client id `dead`, which ChatGPT no longer knows (`invalid_client`): the
/// host id and the chosen model stay, and the next sign-in registers WordCraft again. The caller holds the lock.
fn forget_client(settings: &Path, dead: &str) -> Result<(), String> {
    let client = store::client(settings)?;
    if client.client_id.as_deref() != Some(dead) {
        return Ok(());
    }
    log::warn!("ChatGPT sign-in: ChatGPT no longer knows WordCraft's registration; the next sign-in registers again");
    store::save_client(settings, &Client { client_id: None, ..client })
}

/// Exchange the code for tokens, check the id token, and keep them.
fn exchange(p: &Pending, client_id: &str, code: &str) -> Result<Account, AuthError> {
    let agent = http::auth_agent(p.endpoints.is_local());
    let fields = [
        ("grant_type", "authorization_code"),
        ("client_id", client_id),
        ("code", code),
        ("code_verifier", p.verifier.as_str()),
        ("redirect_uri", p.redirect.as_str()),
        ("resource", p.endpoints.resource.as_str()),
    ];
    let reply = http::post_form(&agent, &p.endpoints.token, &fields).map_err(AuthError::Network)?;
    if !reply.ok() {
        let (code_name, why) = oauth_error(&reply, &[code, &p.verifier]);
        log::warn!("ChatGPT sign-in: the code exchange was refused (HTTP {}, {code_name}, request {:?})", reply.status, reply.request_id);
        return Err(AuthError::Failed(match code_name.as_str() {
            "invalid_grant" => "the sign-in code was refused (it may have been used already): start signing in again".into(),
            "invalid_client" => {
                if let Err(e) = Lock::take(&p.settings).and_then(|_lock| forget_client(&p.settings, client_id)) {
                    log::warn!("ChatGPT sign-in: {e}");
                }
                "ChatGPT no longer knows WordCraft's registration: start signing in again".into()
            }
            _ => format!("ChatGPT refused the sign-in: {why}"),
        }));
    }
    let tokens = Tokens::from(&reply.json(), true)?;
    let claims = check_id_token(&tokens.id_token, &p.endpoints.issuer, client_id, Some(&p.nonce))?;
    let credentials = Credentials {
        version: 0,
        client_id: client_id.to_string(),
        host_id: p.client.host_id.clone(),
        issuer: p.endpoints.issuer.clone(),
        subject: claims.subject,
        email: claims.email,
        name: claims.name,
        id_token: tokens.id_token,
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        token_type: tokens.token_type,
        expires_at: now().saturating_add(tokens.expires_in),
        scopes: tokens.scopes.unwrap_or_else(|| SCOPES.split_whitespace().map(str::to_string).collect()),
        saved_at: 0,
    };
    let lock = Lock::take(&p.settings);
    keep_sign_in(&p.settings, &p.endpoints, credentials, lock)
}

/// Keep one issued sign-in after the credentials lock has been acquired.
pub(super) fn keep_sign_in(
    settings: &Path,
    endpoints: &Endpoints,
    credentials: Credentials,
    lock: Result<Lock, String>,
) -> Result<Account, AuthError> {
    let kept = lock.and_then(|_lock| store::write_credentials(settings, &credentials));
    if let Err(e) = kept {
        revoke_unkept(endpoints, credentials);
        return Err(AuthError::Storage(e));
    }
    let account = credentials.account();
    log::info!("ChatGPT sign-in: signed in (plan usage {})", if account.plan_usage { "allowed" } else { "not allowed" });
    Ok(account)
}

/// An error answer's code and a message without the `secrets`. The code is OAuth's
/// `{"error": "<code>"}`, OpenAI's `{"error": {"code": "<code>", "message": …}}`, or a bare
/// `{"code": "<code>"}`; the message is `error_description`, the error's `message`, or `detail`.
fn oauth_error(reply: &Reply, secrets: &[&str]) -> (String, String) {
    let json = reply.json();
    let nested = json.get("error").filter(|e| e.is_object());
    let code = first_text([json.get("error"), nested.and_then(|e| e.get("code")), json.get("code")]);
    let code: String = code.chars().filter(|c| c.is_ascii_graphic()).take(64).collect();
    let description = first_text([json.get("error_description"), nested.and_then(|e| e.get("message")), json.get("detail")]);
    let why = match (code.is_empty(), description.is_empty()) {
        (true, true) => format!("HTTP {}", reply.status),
        (true, false) => format!("HTTP {}: {description}", reply.status),
        (false, true) => code.clone(),
        (false, false) => format!("{code}: {description}"),
    };
    (code, redact(&why, secrets))
}

/// The first of `candidates` that is text and not empty, else "".
fn first_text(candidates: [Option<&Value>; 3]) -> &str {
    candidates.into_iter().find_map(|v| v.and_then(Value::as_str).filter(|t| !t.is_empty())).unwrap_or_default()
}

/// The tokens a token endpoint answer holds.
struct Tokens {
    access_token: String,
    refresh_token: String,
    id_token: String,
    token_type: String,
    expires_in: u64,
    /// The granted scopes, when the answer named them.
    scopes: Option<Vec<String>>,
}

impl Tokens {
    /// Read them from `json`; `first` (a code exchange) needs a refresh token and an id token,
    /// a refresh may leave them out.
    fn from(json: &Value, first: bool) -> Result<Tokens, AuthError> {
        let token = |name: &str| json.get(name).and_then(Value::as_str).filter(|t| !t.is_empty() && t.len() <= MAX_TOKEN).map(str::to_string);
        let missing = |what: &str| AuthError::Failed(format!("ChatGPT's sign-in answer has no {what}"));
        let access_token = token("access_token").ok_or_else(|| missing("access token"))?;
        let refresh_token = token("refresh_token");
        let id_token = token("id_token");
        if first && refresh_token.is_none() {
            return Err(missing("refresh token"));
        }
        if first && id_token.is_none() {
            return Err(missing("id token"));
        }
        let expires_in = json.get("expires_in").and_then(Value::as_u64).unwrap_or(3600).clamp(30, 7 * 24 * 3600);
        let scopes = json
            .get("scope")
            .and_then(Value::as_str)
            .map(|s| s.split_whitespace().take(64).map(|w| w.chars().take(128).collect::<String>()).collect::<Vec<_>>());
        let token_type = json.get("token_type").and_then(Value::as_str).unwrap_or("Bearer").chars().take(32).collect();
        Ok(Tokens {
            access_token,
            refresh_token: refresh_token.unwrap_or_default(),
            id_token: id_token.unwrap_or_default(),
            token_type,
            expires_in,
            scopes,
        })
    }
}

/// What an id token says about the account.
#[derive(Debug, Default)]
pub(super) struct Claims {
    pub(super) subject: String,
    pub(super) email: String,
    pub(super) name: Option<String>,
}

/// Check the id token's claims: issued by `issuer` for `client_id`, not expired, with `nonce`
/// (when one was sent).
pub(super) fn check_id_token(token: &str, issuer: &str, client_id: &str, nonce: Option<&str>) -> Result<Claims, AuthError> {
    let bad = |why: &str| AuthError::Failed(format!("ChatGPT's sign-in answer can't be trusted: {why}"));
    let mut parts = token.split('.');
    let (Some(_), Some(payload), Some(_), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return Err(bad("the id token isn't a JWT"));
    };
    let claims: Value = from_b64url(payload).and_then(|b| serde_json::from_slice(&b).ok()).ok_or_else(|| bad("the id token can't be read"))?;
    let text = |name: &str| claims.get(name).and_then(Value::as_str);
    if text("iss") != Some(issuer) {
        return Err(bad("another issuer signed it"));
    }
    let audience = match claims.get("aud") {
        Some(Value::String(a)) => a == client_id,
        Some(Value::Array(all)) => all.iter().any(|a| a.as_str() == Some(client_id)),
        _ => false,
    };
    if !audience {
        return Err(bad("it is for another application"));
    }
    let exp = claims.get("exp").and_then(Value::as_u64).ok_or_else(|| bad("it has no expiry"))?;
    if exp.saturating_add(SKEW) < now() {
        return Err(bad("it has expired (is this computer's clock right?)"));
    }
    if claims.get("iat").and_then(Value::as_u64).is_none() {
        return Err(bad("it has no issue time"));
    }
    let subject = text("sub").filter(|s| !s.is_empty()).ok_or_else(|| bad("it names no account"))?;
    if let Some(nonce) = nonce
        && !text("nonce").is_some_and(|n| same_secret(n, nonce))
    {
        return Err(bad("it isn't for this sign-in"));
    }
    if claims.get("sid").is_some_and(|s| s.as_str().is_none_or(str::is_empty)) {
        return Err(bad("its session is empty"));
    }
    let short = |s: &str| s.chars().filter(|c| !c.is_control()).take(320).collect::<String>();
    Ok(Claims { subject: short(subject), email: text("email").map(short).unwrap_or_default(), name: text("name").map(short) })
}

/// The signed-in account's credentials with an access token that has more than five minutes
/// left: the saved one, or a refreshed one. `refused` is a token the API just turned away: a
/// different saved one (another refresh made it) is used as it is; that one is refreshed even
/// before it expires. One refresh runs at a time: one that waited finds the new token saved.
pub(crate) fn access_token(settings: &Path, endpoints: &Endpoints, refused: Option<&str>) -> Result<Credentials, AuthError> {
    let usable = |c: &Credentials| {
        !c.access_token.is_empty() && c.expires_at > now().saturating_add(REFRESH_MARGIN) && refused.is_none_or(|r| r != c.access_token)
    };
    let saved = store::read_credentials(settings).filter(Credentials::signed_in).ok_or(AuthError::NotSignedIn)?;
    if usable(&saved) {
        return Ok(saved);
    }
    let _lock = Lock::take(settings).map_err(AuthError::Failed)?;
    let saved = store::read_credentials(settings).filter(Credentials::signed_in).ok_or(AuthError::NotSignedIn)?;
    if usable(&saved) {
        return Ok(saved);
    }
    refresh(settings, endpoints, saved)
}

/// Refresh `saved`'s tokens (the caller holds the lock) and keep them.
fn refresh(settings: &Path, endpoints: &Endpoints, saved: Credentials) -> Result<Credentials, AuthError> {
    endpoints.check().map_err(AuthError::Failed)?;
    let agent = http::auth_agent(endpoints.is_local());
    let fields = [
        ("grant_type", "refresh_token"),
        ("client_id", saved.client_id.as_str()),
        ("refresh_token", saved.refresh_token.as_str()),
        ("resource", endpoints.resource.as_str()),
    ];
    let reply = http::post_form(&agent, &endpoints.token, &fields).map_err(AuthError::Network)?;
    if !reply.ok() {
        return Err(refused(settings, endpoints, &saved, &reply));
    }
    let tokens = Tokens::from(&reply.json(), false)?;
    // The server has spent the refresh token it was sent: the new tokens are kept, whatever
    // their id token says (unless it names another account).
    let mut fresh = saved.clone();
    fresh.access_token = tokens.access_token;
    if !tokens.refresh_token.is_empty() {
        fresh.refresh_token = tokens.refresh_token;
    }
    fresh.token_type = tokens.token_type;
    fresh.expires_at = now().saturating_add(tokens.expires_in);
    if let Some(scopes) = tokens.scopes {
        fresh.scopes = scopes;
    }
    if !tokens.id_token.is_empty() {
        match check_id_token(&tokens.id_token, &fresh.issuer, &fresh.client_id, None) {
            Ok(claims) if claims.subject != fresh.subject => {
                // Another account's tokens: neither used nor kept (they're revoked), and the
                // sign-in ends.
                revoke_unkept(endpoints, fresh);
                save_over(settings, endpoints, &saved, &ended(&saved))?;
                return Err(AuthError::Failed("ChatGPT's refreshed sign-in is for another account: sign in again (Options › Account)".into()));
            }
            Ok(claims) => {
                fresh.email = claims.email;
                fresh.name = claims.name;
                fresh.id_token = tokens.id_token;
            }
            // The tokens are kept with the last id token and its claims (a clock running ahead
            // makes a new id token look expired).
            Err(e) => log::warn!("ChatGPT sign-in: the refreshed id token isn't kept ({e})"),
        }
    }
    save_over(settings, endpoints, &saved, &fresh)?;
    log::info!("ChatGPT sign-in: refreshed");
    Ok(fresh)
}

/// What a refused refresh means. The sign-in ends when the refresh token is spent or turned away
/// (or the registration is gone, which is forgotten too); it stays through the server's trouble
/// and through other refusals, which say how to start again.
fn refused(settings: &Path, endpoints: &Endpoints, saved: &Credentials, reply: &Reply) -> AuthError {
    let (code, why) = oauth_error(reply, &[&saved.refresh_token, &saved.access_token]);
    log::warn!("ChatGPT sign-in: the refresh was refused (HTTP {}, {code}, request {:?})", reply.status, reply.request_id);
    let registration_gone = code == "invalid_client";
    if registration_gone || SIGN_IN_AGAIN.contains(&code.as_str()) || reply.status == 401 {
        if registration_gone && let Err(e) = forget_client(settings, &saved.client_id) {
            log::warn!("ChatGPT sign-in: {e}");
        }
        // The refresh token is spent: the tokens are forgotten, and who it was is kept (the next
        // sign-in's hints).
        return match save_over(settings, endpoints, saved, &ended(saved)) {
            Ok(()) => AuthError::SignInAgain,
            Err(e) => e,
        };
    }
    if matches!(reply.status, 408 | 429 | 500..=599) {
        // The server's or the network's trouble: the sign-in stays.
        return AuthError::Network(match reply.retry_after {
            Some(wait) => format!("{why} (try again in {} s)", wait.as_secs()),
            None => why,
        });
    }
    AuthError::Failed(format!("ChatGPT didn't renew your sign-in ({why}): try again, or sign out and sign in again in Options › Account"))
}

/// `c` without its tokens: a sign-in that has ended.
fn ended(c: &Credentials) -> Credentials {
    Credentials { access_token: String::new(), refresh_token: String::new(), ..c.clone() }
}

/// Save `next` in place of `used`, the credentials a refresh was sent with, if the file still
/// holds them. When it doesn't (a sign-out or a new sign-in came in between, which only a
/// folder that can't lock files lets happen), `next`'s new refresh token is kept by nobody: it
/// is revoked, and the file stays as it is.
fn save_over(settings: &Path, endpoints: &Endpoints, used: &Credentials, next: &Credentials) -> Result<(), AuthError> {
    let current = store::read_credentials(settings).filter(Credentials::signed_in);
    if current.as_ref().is_some_and(|c| c.client_id == used.client_id && c.refresh_token == used.refresh_token) {
        return store::write_credentials(settings, next).map_err(AuthError::Storage);
    }
    log::warn!("ChatGPT sign-in: the sign-in changed while it was renewed; the renewed tokens aren't kept");
    if next.signed_in() && next.refresh_token != used.refresh_token {
        revoke_unkept(endpoints, next.clone());
    }
    Err(match current {
        None => AuthError::NotSignedIn,
        Some(_) => AuthError::Failed("your ChatGPT sign-in changed while it was renewed: try again".into()),
    })
}

/// How a sign-out went.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignedOut {
    /// Whether ChatGPT confirmed the sign-in is revoked. When it didn't (offline), the tokens
    /// are deleted here anyway; the person can disconnect WordCraft in ChatGPT's settings.
    pub revoked: bool,
}

/// Sign out of OpenAI's ChatGPT: [`sign_out_with`] with OpenAI's endpoints.
pub fn sign_out(settings: &Path) -> Result<SignedOut, AuthError> {
    sign_out_with(settings, &Endpoints::default())
}

/// Sign out: delete the credentials, once a refresh in flight (here or in another WordCraft
/// process) has finished, then revoke their refresh token (at the endpoint the issuer's
/// discovery document names; tried twice). The host id and the issued client id stay, for the
/// next sign-in.
pub fn sign_out_with(settings: &Path, endpoints: &Endpoints) -> Result<SignedOut, AuthError> {
    let signed_in = {
        // Under the lock: the token revoked is the newest, and no refresh starts from it.
        let _lock = Lock::take(settings).map_err(AuthError::Failed)?;
        let saved = store::read_credentials(settings).filter(Credentials::signed_in);
        store::delete_credentials(settings).map_err(AuthError::Storage)?;
        saved
    };
    log::info!("ChatGPT sign-in: signed out");
    let revoked = match signed_in {
        None => true,
        Some(c) => {
            let first = revoke(endpoints, &c);
            let done = first.or_else(|_| {
                std::thread::sleep(Duration::from_secs(1));
                revoke(endpoints, &c)
            });
            if let Err(e) = &done {
                log::warn!("ChatGPT sign-out: the revocation wasn't confirmed: {e}");
            }
            done.is_ok()
        }
    };
    Ok(SignedOut { revoked })
}

/// Revoke `c`'s refresh token at the endpoint the issuer's discovery document names.
fn revoke(endpoints: &Endpoints, c: &Credentials) -> Result<(), String> {
    endpoints.check()?;
    let agent = http::auth_agent(endpoints.is_local());
    let discovery = http::get_json(&agent, &endpoints.discovery, None)?;
    if !discovery.ok() {
        return Err(format!("the sign-in server's configuration couldn't be read (HTTP {})", discovery.status));
    }
    let url = discovery.json().get("revocation_endpoint").and_then(Value::as_str).map(str::to_string).ok_or("no revocation endpoint")?;
    http::check_url(&url)?;
    let issuer_host = format!("{}/", endpoints.issuer.trim_end_matches('/'));
    if !url.starts_with(&issuer_host) && !http::is_loopback(&url) {
        return Err("the revocation endpoint isn't the issuer's".into());
    }
    let fields = [("token", c.refresh_token.as_str()), ("token_type_hint", "refresh_token"), ("client_id", c.client_id.as_str())];
    let reply = http::post_form(&agent, &url, &fields)?;
    if reply.ok() { Ok(()) } else { Err(oauth_error(&reply, &[&c.refresh_token]).1) }
}

/// Revoke `c`'s refresh token on a thread of its own, as well as it can: tokens nobody keeps.
fn revoke_unkept(endpoints: &Endpoints, c: Credentials) {
    let endpoints = endpoints.clone();
    let spawned = std::thread::Builder::new().name("chatgpt revoke".into()).spawn(move || {
        if let Err(e) = revoke(&endpoints, &c) {
            log::warn!("ChatGPT sign-in: tokens nobody keeps weren't revoked: {e}");
        }
    });
    if let Err(e) = spawned {
        log::warn!("ChatGPT sign-in: tokens nobody keeps can't be revoked here: {e}");
    }
}

/// A sign-out running on a thread of its own, for the window: [`SignOut::poll`] each frame.
pub struct SignOut {
    done: Receiver<Result<SignedOut, AuthError>>,
    result: Option<Result<SignedOut, AuthError>>,
}

impl SignOut {
    /// Start signing out of `settings`' account with `endpoints`.
    pub fn start(settings: &Path, endpoints: Endpoints) -> Result<SignOut, AuthError> {
        let (tx, done) = channel();
        let settings = settings.to_path_buf();
        std::thread::Builder::new()
            .name("chatgpt sign-out".into())
            .spawn(move || {
                let _ = tx.send(sign_out_with(&settings, &endpoints));
            })
            .map_err(|e| AuthError::Failed(format!("the sign-out can't start here: {e}")))?;
        Ok(SignOut { done, result: None })
    }

    /// The result, once there is one: never waits.
    pub fn poll(&mut self) -> Option<&Result<SignedOut, AuthError>> {
        if self.result.is_none() {
            match self.done.try_recv() {
                Ok(r) => self.result = Some(r),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.result = Some(Err(AuthError::Failed("the sign-out stopped unexpectedly".into()))),
            }
        }
        self.result.as_ref()
    }

    /// Wait at most `timeout` for the result.
    pub fn wait(&mut self, timeout: Duration) -> Option<&Result<SignedOut, AuthError>> {
        if self.result.is_none() {
            match self.done.recv_timeout(timeout) {
                Ok(r) => self.result = Some(r),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => self.result = Some(Err(AuthError::Failed("the sign-out stopped unexpectedly".into()))),
            }
        }
        self.result.as_ref()
    }
}

/// A model ChatGPT offers the signed-in account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    /// What a request names it (`model`).
    pub slug: String,
    /// Its name in the picker.
    pub name: String,
}

/// Most models read from the list.
const MAX_MODELS: usize = 64;

/// The models the signed-in account may use, from OpenAI's API: [`models_with`] with OpenAI's
/// endpoints. It waits for the network: call it off the window's thread.
pub fn models(settings: &Path) -> Result<Vec<Model>, AuthError> {
    models_with(settings, &Endpoints::default())
}

/// The models the signed-in account may use, in ChatGPT's order (the first is the default). It
/// waits for the network (and a refresh, when the token is old or turned away).
pub fn models_with(settings: &Path, endpoints: &Endpoints) -> Result<Vec<Model>, AuthError> {
    endpoints.check().map_err(AuthError::Failed)?;
    let agent = http::agent(endpoints.is_local(), false);
    let url = format!("{}/models", endpoints.api.trim_end_matches('/'));
    let ask = |refused: Option<&str>| -> Result<(String, Reply), AuthError> {
        let c = access_token(settings, endpoints, refused)?;
        let reply = http::get_json(&agent, &url, Some(&c.access_token)).map_err(AuthError::Network)?;
        Ok((c.access_token, reply))
    };
    let (token, mut reply) = ask(None)?;
    if reply.status == 401 {
        // Turned away though it looks fresh here: refreshed once (the sign-in ends only when
        // the refresh says so), as a turn's token is.
        log::info!("ChatGPT: the models list turned the token away; refreshing it once");
        reply = ask(Some(&token))?.1;
        if reply.status == 401 {
            return Err(AuthError::Failed(
                "ChatGPT didn't accept your sign-in: try again, or sign out and sign in again in Options › Account".into(),
            ));
        }
    }
    if !reply.ok() {
        return Err(AuthError::Failed(format!("ChatGPT's models couldn't be listed (HTTP {})", reply.status)));
    }
    Ok(models_in(&reply.json()))
}

/// The models a models list names: `models` (`slug`, `display_name`, shown when `visibility`
/// is `list` or absent), or the API's usual `data` (`id`).
pub(crate) fn models_in(json: &Value) -> Vec<Model> {
    let list = json.get("models").or_else(|| json.get("data")).and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    list.iter()
        .filter(|m| m.get("visibility").and_then(Value::as_str).is_none_or(|v| v == "list"))
        .filter_map(|m| {
            let slug = m.get("slug").or_else(|| m.get("id")).and_then(Value::as_str)?;
            let ok = !slug.is_empty() && slug.len() <= 128 && slug.chars().all(|c| c.is_ascii_graphic());
            let name = m.get("display_name").and_then(Value::as_str).unwrap_or(slug);
            ok.then(|| Model { slug: slug.to_string(), name: crate::backend::label(name).chars().take(80).collect() })
        })
        .take(MAX_MODELS)
        .collect()
}

/// The model the person chose, if they chose one (else the list's first is used).
pub fn chosen_model(settings: &Path) -> Option<String> {
    store::client(settings).ok().and_then(|c| c.model)
}

/// Choose the model (None: the list's first).
pub fn choose_model(settings: &Path, slug: Option<&str>) -> Result<(), AuthError> {
    let slug = slug.map(str::trim).filter(|s| !s.is_empty());
    if slug.is_some_and(|s| s.len() > 128 || !s.chars().all(|c| c.is_ascii_graphic())) {
        return Err(AuthError::Failed("that isn't a model's name".into()));
    }
    let _lock = Lock::take(settings).map_err(AuthError::Storage)?;
    let client = store::client(settings).map_err(AuthError::Storage)?;
    store::save_client(settings, &Client { model: slug.map(str::to_string), ..client }).map_err(AuthError::Storage)
}

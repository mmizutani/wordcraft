//! Sign in with ChatGPT against fake servers on this machine: the whole flow, refreshes,
//! sign-out, and junk from the browser, the servers and the disk.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::auth::{Claims, access_token, check_id_token, valid_client_id};
use super::fake_server::{Reply, Request, Server};
use super::http::{b64url, from_b64url, now, parse_query};
use super::models::models_in;
use super::store;
use super::vault::{self, Keychain};
use super::*;

pub(super) const WAIT: Duration = Duration::from_secs(20);

pub(super) const ISSUER: &str = "https://auth.openai.com";

/// A settings folder of its own, removed afterwards.
pub(super) struct Folder(PathBuf);

impl Folder {
    pub(super) fn new() -> Folder {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("wordcraft-chatgpt-{}-{n}-{}", std::process::id(), now()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Its key goes to a keychain in memory: tests never reach the system's.
        fake_keychain(&dir, true);
        Folder(dir)
    }

    pub(super) fn path(&self) -> &Path {
        &self.0
    }

    /// The files in it.
    fn files(&self) -> Vec<String> {
        let mut all: Vec<String> = std::fs::read_dir(&self.0).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        all.sort();
        all
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// An id token as the token endpoint gives it (its signature isn't checked).
pub(super) fn id_token(claims: &Value) -> String {
    format!("{}.{}.{}", b64url(br#"{"alg":"RS256","typ":"JWT"}"#), b64url(claims.to_string().as_bytes()), b64url(b"signature"))
}

pub(super) fn claims(client_id: &str, nonce: Option<&str>) -> Value {
    let mut c = json!({
        "iss": ISSUER, "aud": [client_id], "sub": "user-123", "exp": now() + 3600, "iat": now(),
        "email": "writer@example.com", "name": "A Writer", "sid": "session-1",
    });
    if let Some(n) = nonce {
        c["nonce"] = json!(n);
    }
    c
}

/// What the fake sign-in server does.
#[derive(Default)]
pub(super) struct Script {
    /// The nonce the token endpoint puts in the id token (the sign-in's).
    pub(super) nonce: Mutex<String>,
    /// The scopes the token endpoint grants, when not all of them.
    pub(super) scope: Mutex<Option<String>>,
    /// Omit scope when all requested scopes were granted.
    pub(super) omit_scope: Mutex<bool>,
    /// What the token endpoint answers instead of tokens.
    pub(super) refuse: Mutex<Option<(u16, Value)>>,
    /// How long the token endpoint takes.
    pub(super) slow: Mutex<Duration>,
    /// What the revocation endpoint answers (200 when 0).
    pub(super) revoke: Mutex<u16>,
    /// Access tokens the models list turns away (401), as a server that no longer takes them.
    pub(super) turned_away: Mutex<Vec<String>>,
    /// What the models list answers, when not the two test models.
    pub(super) models: Mutex<Option<Reply>>,
}

/// The fake sign-in server: the token, discovery, revocation and models endpoints, with
/// `api` answering everything under `/v1/` that isn't the models list.
pub(super) fn server(script: Arc<Script>, api: impl Fn(&Request, usize) -> Reply + Send + Sync + 'static) -> Server {
    Server::start(move |r, n| match (r.method.as_str(), r.path.as_str()) {
        ("POST", "/oauth/token") => token(&script, r, n),
        ("GET", "/.well-known/openid-configuration") => Reply::json(
            200,
            &json!({"issuer": ISSUER, "revocation_endpoint": format!("http://{}/oauth/revoke", r.header("host").unwrap_or_default())}),
        ),
        ("POST", "/oauth/revoke") => Reply::empty(match *script.revoke.lock().unwrap() {
            0 => 200,
            s => s,
        }),
        ("GET", "/v1/models") if turned_away(&script, r) => {
            Reply::json(401, &json!({"error": {"code": "subscription_sharing_invalid_user", "message": "who are you"}}))
        }
        ("GET", "/v1/models") if script.models.lock().unwrap().is_some() => script.models.lock().unwrap().clone().unwrap(),
        ("GET", "/v1/models") => Reply::json(
            200,
            &json!({"models": [
                {"slug": "gpt-hidden", "display_name": "Hidden", "visibility": "hide"},
                {"slug": "gpt-test", "display_name": "GPT Test", "visibility": "list"},
                {"slug": "gpt-test-mini", "display_name": "GPT Test mini", "visibility": "list"},
            ]}),
        ),
        _ if r.path.starts_with("/v1/") => api(r, n),
        _ => Reply::json(404, &json!({"detail": "Not Found"})),
    })
}

/// Whether `r` carries an access token the script turns away.
fn turned_away(script: &Script, r: &Request) -> bool {
    let sent = r.header("authorization").and_then(|h| h.strip_prefix("Bearer "));
    sent.is_some_and(|t| script.turned_away.lock().unwrap().iter().any(|away| away == t))
}

fn token(script: &Script, r: &Request, n: usize) -> Reply {
    std::thread::sleep(*script.slow.lock().unwrap());
    if let Some((status, body)) = script.refuse.lock().unwrap().clone() {
        return Reply::json(status, &body);
    }
    let client = r.field("client_id").unwrap_or_default();
    let first = r.field("grant_type").as_deref() == Some("authorization_code");
    let nonce = script.nonce.lock().unwrap().clone();
    let scope = script.scope.lock().unwrap().clone().unwrap_or_else(|| SCOPES.to_string());
    let mut body = json!({
        "access_token": format!("access-{n}"),
        "refresh_token": format!("refresh-{n}"),
        "id_token": id_token(&claims(&client, first.then_some(nonce.as_str()))),
        "token_type": "Bearer",
        "expires_in": 3600,
        "scope": scope,
    });
    if *script.omit_scope.lock().unwrap() {
        body.as_object_mut().unwrap().remove("scope");
    }
    Reply::json(200, &body)
}

pub(super) fn options(endpoints: &Endpoints) -> SignInOptions {
    SignInOptions { endpoints: endpoints.clone(), port: 0, wait: Duration::from_secs(120), consent: false }
}

/// The sign-in address's parameter `name`.
fn param(url: &str, name: &str) -> Option<String> {
    parse_query(url.split_once('?').map_or("", |(_, q)| q)).into_iter().find(|(k, _)| k == name).map(|(_, v)| v)
}

/// The browser comes back to `redirect` with `query`: the status and the page.
fn callback(redirect: &str, query: &str) -> (u16, String) {
    let (host, path) = redirect.strip_prefix("http://").unwrap().split_once('/').unwrap();
    raw(host, format!("GET /{path}?{query} HTTP/1.1\r\nHost: {host}\r\n\r\n").as_bytes())
}

/// Send `bytes` to `host` and read what comes back: the status (0 when none) and the text.
fn raw(host: &str, bytes: &[u8]) -> (u16, String) {
    let mut s = TcpStream::connect(host).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let _ = s.write_all(bytes);
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    let text = String::from_utf8_lossy(&out).into_owned();
    (text.split(' ').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0), text)
}

/// Sign in to `endpoints` from `folder`, the browser bringing `query` (the right state is added).
fn sign_in_with(folder: &Path, script: &Script, endpoints: &Endpoints, query: &str) -> Result<Account, AuthError> {
    let mut s = SignIn::start(folder, options(endpoints)).unwrap();
    let url = s.url().to_string();
    *script.nonce.lock().unwrap() = param(&url, "nonce").unwrap();
    let _ = callback(s.redirect_uri(), &format!("{query}&state={}", param(&url, "state").unwrap()));
    s.wait(WAIT).cloned().unwrap()
}

/// A folder signed in (registration done, client `oaiapp_issued-1`) to a fake server.
pub(super) fn signed_in(api: impl Fn(&Request, usize) -> Reply + Send + Sync + 'static) -> (Folder, Arc<Script>, Server, Endpoints) {
    let folder = Folder::new();
    let script = Arc::new(Script::default());
    let server = server(script.clone(), api);
    let endpoints = Endpoints::local(server.base());
    sign_in_with(folder.path(), &script, &endpoints, "code=the-code&client_id=oaiapp_issued-1").unwrap();
    (folder, script, server, endpoints)
}

pub(super) fn no_api(_: &Request, _: usize) -> Reply {
    Reply::json(404, &json!({"detail": "Not Found"}))
}

/// Make the saved access token one that expires in a minute.
pub(super) fn expire(settings: &Path) {
    let mut c = store::read_credentials(settings).unwrap();
    c.expires_at = now() + 60;
    store::write_credentials(settings, &c).unwrap();
}

/// Wait (at most [`WAIT`]) until `done` holds.
pub(super) fn eventually(what: &str, done: impl Fn() -> bool) {
    let until = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < until, "never: {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The lock on `settings`' sign-in, as another WordCraft process holds it while it refreshes (a
/// second handle stands in for that process: the system's locks on a file keep two handles
/// apart). It has held it for two minutes: a live holder is waited for, however long it takes.
fn hold_lock(settings: &Path) -> File {
    let file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(settings.join(store::LOCK_FILE)).unwrap();
    file.lock().unwrap();
    file.set_modified(SystemTime::now() - Duration::from_secs(120)).unwrap();
    file
}

/// Whether somebody holds the lock file at `path`.
fn held(path: &Path) -> bool {
    match OpenOptions::new().read(true).write(true).open(path).unwrap().try_lock() {
        Ok(()) => false,
        Err(TryLockError::WouldBlock) => true,
        Err(TryLockError::Error(e)) => panic!("{e}"),
    }
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// The address of a first sign-in: a registration, with this installation's host id, PKCE
/// (S256), a fresh state and nonce, and a loopback redirect.
fn assert_registration(url: &str, base: &str, host_id: &str) {
    assert!(url.starts_with(&format!("{base}/authorize?")), "{url}");
    assert!(host_id.starts_with("urn:uuid:") && host_id.len() == 45, "{host_id}");
    let expected = [
        ("client_id", "dynamic_agent_client"),
        ("agent_name_hint", "WordCraft"),
        ("ext_agent_host_id", host_id),
        ("response_type", "code"),
        ("scope", SCOPES),
        ("resource", RESOURCE),
        ("code_challenge_method", "S256"),
    ];
    for (name, value) in expected {
        assert_eq!(param(url, name).as_deref(), Some(value), "{name}");
    }
    for name in ["id_token_hint", "login_hint", "prompt"] {
        assert!(param(url, name).is_none(), "{name}");
    }
    let redirect = param(url, "redirect_uri").unwrap();
    assert!(redirect.starts_with("http://127.0.0.1:") && redirect.ends_with("/auth/callback"), "{redirect}");
    let (state, nonce) = (param(url, "state").unwrap(), param(url, "nonce").unwrap());
    assert!(state.len() >= 40 && nonce.len() >= 40 && state != nonce);
}

/// The code exchange: the verifier for the address's challenge, the same redirect address, the
/// issued client, no secret.
fn assert_exchange(exchange: &Request, url: &str) {
    let verifier = exchange.field("code_verifier").unwrap();
    assert!(verifier.len() >= 43);
    assert_eq!(b64url(Sha256::digest(verifier.as_bytes()).as_slice()), param(url, "code_challenge").unwrap());
    let expected = [
        ("grant_type", "authorization_code"),
        ("client_id", "oaiapp_issued-1"),
        ("code", "the-code"),
        ("redirect_uri", param(url, "redirect_uri").unwrap().as_str()),
        ("resource", RESOURCE),
    ]
    .map(|(k, v)| (k, v.to_string()));
    for (name, value) in expected {
        assert_eq!(exchange.field(name), Some(value), "{name}");
    }
    assert_eq!(exchange.fields().len(), 6, "{:?}", exchange.fields());
    assert_eq!(exchange.header("content-type"), Some("application/x-www-form-urlencoded"));
}

/// What a sign-in keeps: the issued client id with the host id, and the tokens, sealed, in files
/// only their owner reads, written whole.
fn assert_kept(folder: &Folder, host_id: &str) {
    let client = store::client(folder.path()).unwrap();
    assert_eq!((client.host_id.as_str(), client.client_id.as_deref()), (host_id, Some("oaiapp_issued-1")));
    let saved = store::read_credentials(folder.path()).unwrap();
    assert_eq!((saved.access_token.as_str(), saved.refresh_token.as_str()), ("access-0", "refresh-0"));
    assert_eq!((saved.subject.as_str(), saved.issuer.as_str()), ("user-123", ISSUER));
    assert!(saved.plan_usage() && saved.expires_at > now() + 3000 && saved.id_token.split('.').count() == 3);
    // On disk the tokens, and who they are for, are sealed.
    let sealed = std::fs::read_to_string(folder.path().join(CREDENTIALS_FILE)).unwrap();
    for secret in ["access-0", "refresh-0", "writer@example.com", "oaiapp_issued-1", "eyJ"] {
        assert!(!sealed.contains(secret), "{secret} is readable in {sealed}");
    }
    // The lock the tokens were saved under stays: empty, and let go of.
    assert_eq!(folder.files(), [CLIENT_FILE, store::LOCK_FILE, CREDENTIALS_FILE]);
    let lock = folder.path().join(store::LOCK_FILE);
    assert!(std::fs::metadata(&lock).unwrap().len() == 0 && !held(&lock));
    #[cfg(unix)]
    {
        assert_eq!(mode(&folder.path().join(CREDENTIALS_FILE)), 0o600);
        assert_eq!(mode(&folder.path().join(CLIENT_FILE)), 0o600);
        assert_eq!(mode(&lock), 0o600);
    }
}

#[test]
fn signing_in_registers_wordcraft_exchanges_the_code_and_keeps_the_tokens_private() {
    let folder = Folder::new();
    let script = Arc::new(Script::default());
    let server = server(script.clone(), no_api);
    let endpoints = Endpoints::local(server.base());
    let mut sign_in = SignIn::start(folder.path(), options(&endpoints)).unwrap();
    let url = sign_in.url().to_string();
    // The host id is kept before the sign-in.
    let host_id = store::client(folder.path()).unwrap().host_id;
    assert_registration(&url, server.base(), &host_id);
    let redirect = param(&url, "redirect_uri").unwrap();
    assert_eq!(sign_in.redirect_uri(), redirect);
    let state = param(&url, "state").unwrap();
    *script.nonce.lock().unwrap() = param(&url, "nonce").unwrap();
    assert!(sign_in.poll().is_none(), "it waits for the browser");
    // An answer that isn't this sign-in's (another state) is refused, and the sign-in waits on.
    let (status, page) = callback(&redirect, "code=stolen&state=wrong&client_id=oaiapp_evil");
    assert_eq!(status, 400, "{page}");
    assert!(sign_in.wait(Duration::from_millis(200)).is_none());
    assert!(server.to("/oauth/token").is_empty());
    // The browser comes back with the code and the client id WordCraft was issued.
    let (status, page) = callback(&redirect, &format!("code=the-code&scope=openid+email&state={state}&client_id=oaiapp_issued-1"));
    assert_eq!(status, 200, "{page}");
    assert!(page.contains("signed in") && page.contains("Cache-Control: no-store"), "{page}");
    let account = sign_in.wait(WAIT).cloned().unwrap().unwrap();
    assert_eq!(account, Account { email: "writer@example.com".into(), name: Some("A Writer".into()), plan_usage: true, kept: Kept::Keychain });
    assert_exchange(&server.to("/oauth/token")[0], &url);
    assert_kept(&folder, &host_id);
    assert_eq!(super::account(folder.path()), Some(account.clone()));
    assert_eq!(readiness(Some(folder.path())), Readiness::Ready);
    // Nothing the window may log or show holds a token.
    let saved = store::read_credentials(folder.path()).unwrap();
    let shown = format!("{sign_in:?} {saved:?} {account:?} {}", serde_json::to_string(&account).unwrap());
    for secret in ["access-0", "refresh-0", "the-code", "eyJ"] {
        assert!(!shown.contains(secret), "{shown}");
    }
}

#[test]
fn sign_in_without_returned_scopes_keeps_the_requested_scopes() {
    let folder = Folder::new();
    let script = Arc::new(Script::default());
    *script.omit_scope.lock().unwrap() = true;
    let server = server(script.clone(), no_api);
    let endpoints = Endpoints::local(server.base());
    let account = sign_in_with(folder.path(), &script, &endpoints, "code=c&client_id=oaiapp_issued-1").unwrap();
    assert!(account.plan_usage);
    assert_eq!(readiness(Some(folder.path())), Readiness::Ready);
    assert_eq!(store::read_credentials(folder.path()).unwrap().scopes, SCOPES.split_whitespace().collect::<Vec<_>>());
}

#[test]
fn an_unsaved_sign_in_preserves_credentials_when_the_lock_times_out_and_revokes_new_tokens() {
    let (folder, _script, server, endpoints) = signed_in(no_api);
    let old = store::read_credentials(folder.path()).unwrap();
    let mut fresh = old.clone();
    fresh.access_token = "new-access".into();
    fresh.refresh_token = "unkept-refresh".into();
    let holder = hold_lock(folder.path());
    let lock = store::Lock::within(folder.path(), Duration::ZERO);
    assert!(lock.is_err(), "the system file lock is actually held");
    let result = super::auth::keep_sign_in(folder.path(), &endpoints, fresh, lock);
    assert!(matches!(result, Err(AuthError::Storage(_))));
    assert_eq!(store::read_credentials(folder.path()), Some(old));
    assert!(held(&folder.path().join(store::LOCK_FILE)), "the live holder was not displaced");
    eventually("unkept tokens revoked after lock failure", || {
        server.to("/oauth/revoke").iter().any(|r| r.field("token").as_deref() == Some("unkept-refresh"))
    });
    drop(holder);
}

#[test]
fn an_unsaved_sign_in_revokes_new_tokens_when_the_credentials_cannot_be_written() {
    let (folder, _script, server, endpoints) = signed_in(no_api);
    let mut fresh = store::read_credentials(folder.path()).unwrap();
    fresh.refresh_token = "unwritten-refresh".into();
    let path = folder.path().join(CREDENTIALS_FILE);
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let lock = store::Lock::take(folder.path());
    let result = super::auth::keep_sign_in(folder.path(), &endpoints, fresh, lock);
    assert!(matches!(result, Err(AuthError::Storage(_))));
    assert!(path.is_dir(), "the failed save left the existing filesystem entry alone");
    eventually("unwritten tokens revoked after storage failure", || {
        server.to("/oauth/revoke").iter().any(|r| r.field("token").as_deref() == Some("unwritten-refresh"))
    });
}

#[test]
fn signing_in_again_reuses_the_issued_client_with_hints_and_refuses_another_registration() {
    let (folder, script, server, endpoints) = signed_in(no_api);
    let host_id = store::client(folder.path()).unwrap().host_id;
    // Asking again for plan usage: the saved client, the hints, and consent asked for.
    let mut again = SignIn::start(folder.path(), SignInOptions { consent: true, ..options(&endpoints) }).unwrap();
    let url = again.url().to_string();
    assert_eq!(param(&url, "client_id").as_deref(), Some("oaiapp_issued-1"));
    assert_eq!(param(&url, "ext_agent_host_id"), Some(host_id));
    assert!(param(&url, "agent_name_hint").is_none());
    assert!(param(&url, "id_token_hint").is_some_and(|t| t.split('.').count() == 3));
    assert_eq!(param(&url, "login_hint").as_deref(), Some("writer@example.com"));
    assert_eq!(param(&url, "prompt").as_deref(), Some("consent"));
    // An answer for another registration is refused, and nothing is exchanged.
    let state = param(&url, "state").unwrap();
    let (status, page) = callback(again.redirect_uri(), &format!("code=c&state={state}&client_id=oaiapp_other"));
    assert_eq!(status, 400, "{page}");
    assert!(matches!(again.wait(WAIT), Some(Err(AuthError::Failed(why))) if why.contains("another WordCraft registration")), "{:?}", again.poll());
    assert_eq!(server.to("/oauth/token").len(), 1);
    // An ordinary sign-in doesn't ask for consent, and its answer may leave the client id out.
    let mut third = SignIn::start(folder.path(), options(&endpoints)).unwrap();
    let url = third.url().to_string();
    assert!(param(&url, "prompt").is_none());
    *script.nonce.lock().unwrap() = param(&url, "nonce").unwrap();
    let (status, _) = callback(third.redirect_uri(), &format!("code=c2&state={}", param(&url, "state").unwrap()));
    assert_eq!(status, 200);
    assert!(third.wait(WAIT).unwrap().is_ok());
    let exchange = server.to("/oauth/token").pop().unwrap();
    assert_eq!((exchange.field("client_id").as_deref(), exchange.field("code").as_deref()), (Some("oaiapp_issued-1"), Some("c2")));
    assert_eq!(store::read_credentials(folder.path()).unwrap().access_token, "access-1");
}

#[test]
fn a_declined_refused_or_untrustworthy_sign_in_ends_plainly_and_keeps_no_tokens() {
    let folder = Folder::new();
    let script = Arc::new(Script::default());
    let server = server(script.clone(), no_api);
    let endpoints = Endpoints::local(server.base());
    let attempt = |query: &str| sign_in_with(folder.path(), &script, &endpoints, query);
    // Plan usage declined: no exchange.
    assert_eq!(attempt("error=access_denied&error_description=The+user+declined"), Err(AuthError::Denied));
    assert!(AuthError::Denied.to_string().contains("Plus or Pro"));
    assert!(server.to("/oauth/token").is_empty());
    // Another error, with a description that tries to put a token on the page.
    let e = attempt(&format!("error=server_error&error_description=bad+{}", "A".repeat(60))).unwrap_err().to_string();
    assert!(e.contains("server_error") && !e.contains(&"A".repeat(60)), "{e}");
    // A registration answer without the issued client id, or with junk for one.
    for query in ["code=c", "code=c&client_id=dynamic_agent_client", "code=c&client_id=oaiapp_", "code=c&client_id=oaiapp_%0A%00"] {
        let e = attempt(query).unwrap_err();
        assert!(e.to_string().contains("registration"), "{query}: {e}");
    }
    assert_eq!(store::client(folder.path()).unwrap().client_id, None, "nothing but an issued client id is kept");
    // No code.
    assert!(attempt("client_id=oaiapp_issued-1").unwrap_err().to_string().contains("no code"));
    // The token endpoint refuses the code, or fails.
    for refusal in [
        json!({"error": "invalid_grant", "error_description": "code the-code was used"}),
        json!({"error": {"code": "invalid_grant", "message": "code the-code was used"}}),
    ] {
        *script.refuse.lock().unwrap() = Some((400, refusal));
        let e = attempt("code=the-code&client_id=oaiapp_issued-1").unwrap_err().to_string();
        assert!(e.contains("start signing in again") && !e.contains("the-code"), "{e}");
    }
    *script.refuse.lock().unwrap() = Some((500, json!("<html>oops</html>")));
    assert!(attempt("code=c&client_id=oaiapp_issued-1").unwrap_err().to_string().contains("HTTP 500"));
    *script.refuse.lock().unwrap() = Some((200, json!({"access_token": "a", "token_type": "Bearer"})));
    assert!(attempt("code=c&client_id=oaiapp_issued-1").unwrap_err().to_string().contains("no refresh token"));
    *script.refuse.lock().unwrap() = None;
    // An id token for another sign-in (its nonce isn't this one's).
    let mut s = SignIn::start(folder.path(), options(&endpoints)).unwrap();
    let url = s.url().to_string();
    *script.nonce.lock().unwrap() = "another-sign-in-s-nonce".into();
    let _ = callback(s.redirect_uri(), &format!("code=c&state={}", param(&url, "state").unwrap()));
    assert!(matches!(s.wait(WAIT), Some(Err(AuthError::Failed(why))) if why.contains("can't be trusted")));
    // None of it signed anyone in.
    assert!(store::read_credentials(folder.path()).is_none());
    assert!(matches!(readiness(Some(folder.path())), Readiness::SignIn { .. }));
}

#[test]
fn junk_and_huge_requests_on_the_sign_in_port_are_answered_and_ignored() {
    let folder = Folder::new();
    let script = Arc::new(Script::default());
    let server = server(script.clone(), no_api);
    let endpoints = Endpoints::local(server.base());
    let mut sign_in = SignIn::start(folder.path(), options(&endpoints)).unwrap();
    let url = sign_in.url().to_string();
    let redirect = sign_in.redirect_uri().to_string();
    let host = redirect.strip_prefix("http://").unwrap().split_once('/').unwrap().0.to_string();
    let state = param(&url, "state").unwrap();
    assert_eq!(raw(&host, b"\x00\xff\xfe junk\r\n\r\n").0, 404);
    assert_eq!(raw(&host, b"GET /favicon.ico HTTP/1.1\r\n\r\n").0, 404);
    assert_eq!(raw(&host, format!("POST /auth/callback?code=c&state={state} HTTP/1.1\r\n\r\n").as_bytes()).0, 405);
    assert_eq!(raw(&host, b"GET /auth/callback HTTP/1.1\r\n\r\n").0, 400);
    assert_eq!(raw(&host, b"GET /auth/callback?state=%ZZ%&code=%00%ff&state HTTP/1.1\r\n\r\n").0, 400);
    assert_eq!(raw(&host, format!("GET /auth/callback?state={}x HTTP/1.1\r\n\r\n", state).as_bytes()).0, 400);
    // A request line longer than the sign-in reads, and a megabyte with no line at all.
    let long = format!("GET /auth/callback?code=c&{}&state={state} HTTP/1.1\r\n\r\n", "a=b&".repeat(10_000));
    assert!(matches!(raw(&host, long.as_bytes()).0, 0 | 400));
    assert!(matches!(raw(&host, &vec![b'A'; 1 << 20]).0, 0 | 400));
    // Connections that say nothing, or half a line, and go away.
    drop(TcpStream::connect(&host).unwrap());
    let mut half = TcpStream::connect(&host).unwrap();
    half.write_all(b"GET /auth/cal").unwrap();
    drop(half);
    assert!(sign_in.poll().is_none(), "the sign-in goes on: {:?}", sign_in.poll());
    assert!(server.to("/oauth/token").is_empty());
    // The browser's real answer still signs in.
    *script.nonce.lock().unwrap() = param(&url, "nonce").unwrap();
    let (status, _) = callback(&redirect, &format!("code=c&state={state}&client_id=oaiapp_issued-1"));
    assert_eq!(status, 200);
    assert!(sign_in.wait(WAIT).unwrap().is_ok());
}

#[test]
fn a_sign_in_can_be_cancelled_times_out_and_finds_a_free_port() {
    let folder = Folder::new();
    let endpoints = Endpoints::local("http://127.0.0.1:9");
    let mut s = SignIn::start(folder.path(), options(&endpoints)).unwrap();
    s.cancel();
    assert_eq!(s.wait(WAIT).cloned(), Some(Err(AuthError::Cancelled)));
    assert_eq!(s.poll().cloned(), Some(Err(AuthError::Cancelled)), "the result stays");
    let mut t = SignIn::start(folder.path(), SignInOptions { wait: Duration::from_millis(100), ..options(&endpoints) }).unwrap();
    assert_eq!(t.wait(WAIT).cloned(), Some(Err(AuthError::TimedOut(1))));
    assert!(AuthError::TimedOut(1).to_string().contains("in time (1 min)"));
    // A taken port: another one.
    let taken = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    let u = SignIn::start(folder.path(), SignInOptions { port, ..options(&endpoints) }).unwrap();
    assert!(!u.redirect_uri().contains(&format!(":{port}/")), "{}", u.redirect_uri());
    assert_eq!((SignInOptions::default().port, DEFAULT_PORT, CALLBACK_PATH), (1455, 1455, "/auth/callback"));
    assert_eq!(SignInOptions::default().endpoints, Endpoints::default());
    // Endpoints that would send a token over the network unencrypted are refused.
    let plain = Endpoints { token: "http://auth.example.com/oauth/token".into(), ..Endpoints::default() };
    assert!(matches!(SignIn::start(folder.path(), options(&plain)), Err(AuthError::Failed(_))));
}

#[test]
fn a_flood_of_junk_or_idle_connections_on_the_sign_in_port_neither_ends_nor_holds_back_the_sign_in() {
    let folder = Folder::new();
    let script = Arc::new(Script::default());
    let server = server(script.clone(), no_api);
    let endpoints = Endpoints::local(server.base());
    let mut sign_in = SignIn::start(folder.path(), options(&endpoints)).unwrap();
    let url = sign_in.url().to_string();
    let redirect = sign_in.redirect_uri().to_string();
    let host = redirect.strip_prefix("http://").unwrap().split_once('/').unwrap().0.to_string();
    // More requests than the port once answered (256): unknown paths, and answers with another
    // state.
    for i in 0..300 {
        let (status, _) = if i % 2 == 0 { raw(&host, b"GET /x HTTP/1.1\r\n\r\n") } else { callback(&redirect, "code=stolen&state=wrong") };
        assert!(status == 404 || status == 400, "request {i} wasn't answered: {status}");
    }
    assert!(sign_in.poll().is_none(), "the sign-in goes on: {:?}", sign_in.poll());
    // Connections that say nothing and stay open don't hold back the browser's answer.
    let idle: Vec<TcpStream> = (0..8).map(|_| TcpStream::connect(&host).unwrap()).collect();
    *script.nonce.lock().unwrap() = param(&url, "nonce").unwrap();
    let (status, page) = callback(&redirect, &format!("code=c&state={}&client_id=oaiapp_issued-1", param(&url, "state").unwrap()));
    assert_eq!(status, 200, "{page}");
    assert!(sign_in.wait(WAIT).unwrap().is_ok());
    drop(idle);
}

#[test]
fn a_sign_in_takes_any_wait() {
    let folder = Folder::new();
    let endpoints = Endpoints::local("http://127.0.0.1:9");
    // However long (it panicked on Duration::MAX): it waits, and cancelling ends it.
    let mut s = SignIn::start(folder.path(), SignInOptions { wait: Duration::MAX, ..options(&endpoints) }).unwrap();
    assert!(s.wait(Duration::from_millis(200)).is_none(), "an endless wait doesn't end at once");
    s.cancel();
    assert_eq!(s.wait(WAIT).cloned(), Some(Err(AuthError::Cancelled)));
}

#[test]
fn an_old_token_is_refreshed_once_for_two_callers_and_the_rotated_token_kept() {
    let (folder, script, server, endpoints) = signed_in(no_api);
    // Fresh enough: no refresh.
    assert_eq!(access_token(folder.path(), &endpoints, None).unwrap().access_token, "access-0");
    assert_eq!(server.to("/oauth/token").len(), 1);
    // About to expire: two callers at once make one refresh, and both get its token.
    expire(folder.path());
    *script.slow.lock().unwrap() = Duration::from_millis(300);
    let callers: Vec<_> = (0..2)
        .map(|_| {
            let (f, e) = (folder.path().to_path_buf(), endpoints.clone());
            std::thread::spawn(move || access_token(&f, &e, None).unwrap().access_token)
        })
        .collect();
    let got: Vec<String> = callers.into_iter().map(|c| c.join().unwrap()).collect();
    assert_eq!(got, ["access-1", "access-1"]);
    let all = server.to("/oauth/token");
    assert_eq!(all.len(), 2, "one exchange, one refresh");
    let r = &all[1];
    assert_eq!(r.field("grant_type").as_deref(), Some("refresh_token"));
    assert_eq!(r.field("refresh_token").as_deref(), Some("refresh-0"));
    assert_eq!(r.field("client_id").as_deref(), Some("oaiapp_issued-1"));
    assert_eq!(r.field("resource").as_deref(), Some(RESOURCE));
    assert_eq!(r.fields().len(), 4, "no scope: {:?}", r.fields());
    // The rotated refresh token is the one kept, with the new access token and expiry.
    let saved = store::read_credentials(folder.path()).unwrap();
    assert_eq!((saved.access_token.as_str(), saved.refresh_token.as_str()), ("access-1", "refresh-1"));
    assert!(saved.expires_at > now() + 3000);
    assert!(!held(&folder.path().join(store::LOCK_FILE)), "the lock is let go of");
    // A token the API turned away is refreshed though it hasn't expired; once another caller
    // has, the newer one is used as it is.
    *script.slow.lock().unwrap() = Duration::ZERO;
    assert_eq!(access_token(folder.path(), &endpoints, Some("access-1")).unwrap().access_token, "access-2");
    assert_eq!(access_token(folder.path(), &endpoints, Some("access-1")).unwrap().access_token, "access-2");
    assert_eq!(server.to("/oauth/token").len(), 3);
}

#[test]
fn trouble_on_the_server_or_another_refusal_keeps_the_sign_in() {
    let (folder, script, _server, endpoints) = signed_in(no_api);
    expire(folder.path());
    // The server's trouble, or too many requests: the sign-in stays, and the error says so.
    for (status, body, code) in [
        (503, json!({"error": "temporarily_unavailable"}), "temporarily_unavailable"),
        (429, json!({"error": {"code": "rate_limit_exceeded", "message": "slow down"}}), "rate_limit_exceeded: slow down"),
    ] {
        *script.refuse.lock().unwrap() = Some((status, body));
        let e = access_token(folder.path(), &endpoints, None).unwrap_err();
        assert!(matches!(&e, AuthError::Network(why) if why.contains(code)), "{e:?}");
        assert!(super::account(folder.path()).is_some());
    }
    // Another refusal keeps it too, and says how to start again, without the token.
    *script.refuse.lock().unwrap() = Some((400, json!({"error": "invalid_request", "error_description": "refresh-0 isn't for this resource"})));
    let e = access_token(folder.path(), &endpoints, None).unwrap_err();
    assert!(matches!(&e, AuthError::Failed(why) if why.contains("invalid_request") && why.contains("sign in again")), "{e:?}");
    assert!(!e.to_string().contains("refresh-0"), "{e}");
    assert!(super::account(folder.path()).is_some());
}

#[test]
fn a_spent_or_turned_away_refresh_token_ends_the_sign_in_whatever_shape_the_refusal_has() {
    let (folder, script, server, endpoints) = signed_in(no_api);
    expire(folder.path());
    // OAuth's shape, OpenAI's error envelope, a bare code, or a 401 with no code at all.
    let spent = [
        (400, json!({"error": "invalid_grant", "error_description": "refresh-0 is spent"})),
        (400, json!({"error": "refresh_token_reused"})),
        (
            401,
            json!({"error": {"message": "Your refresh token has already been used to generate a new access token. Please try signing in again.",
                             "type": "invalid_request_error", "param": null, "code": "refresh_token_reused"}}),
        ),
        (401, json!({"error": {"code": "refresh_token_expired", "message": "refresh-0 has expired"}})),
        (400, json!({"code": "refresh_token_invalidated"})),
        (401, json!({"detail": "Unauthorized"})),
    ];
    for (status, body) in spent {
        *script.refuse.lock().unwrap() = Some((status, body.clone()));
        let mut c = store::read_credentials(folder.path()).unwrap();
        c.refresh_token = "refresh-0".into();
        store::write_credentials(folder.path(), &c).unwrap();
        assert_eq!(access_token(folder.path(), &endpoints, None).unwrap_err(), AuthError::SignInAgain, "{body}");
        assert_eq!(super::account(folder.path()), None, "{body}");
        assert!(matches!(readiness(Some(folder.path())), Readiness::SignIn { .. }));
        assert_eq!(access_token(folder.path(), &endpoints, None).unwrap_err(), AuthError::NotSignedIn);
    }
    // The next sign-in still has the issued client and the hints.
    *script.refuse.lock().unwrap() = None;
    let next = SignIn::start(folder.path(), options(&endpoints)).unwrap();
    assert_eq!(param(next.url(), "client_id").as_deref(), Some("oaiapp_issued-1"));
    assert!(param(next.url(), "id_token_hint").is_some());
    drop(server);
}

#[test]
fn a_registration_chatgpt_no_longer_knows_is_forgotten_and_signing_in_again_registers_anew() {
    let (folder, script, server, endpoints) = signed_in(no_api);
    let host_id = store::client(folder.path()).unwrap().host_id;
    expire(folder.path());
    *script.refuse.lock().unwrap() = Some((401, json!({"error": "invalid_client"})));
    assert_eq!(access_token(folder.path(), &endpoints, None).unwrap_err(), AuthError::SignInAgain);
    // The refused client id is forgotten, with the sign-in; the host id stays.
    let client = store::client(folder.path()).unwrap();
    assert_eq!((client.host_id.as_str(), client.client_id.as_deref()), (host_id.as_str(), None));
    assert_eq!(super::account(folder.path()), None);
    // Signing in again, as the error says, registers WordCraft anew (without the last sign-in's
    // hints) and works.
    *script.refuse.lock().unwrap() = None;
    let next = SignIn::start(folder.path(), options(&endpoints)).unwrap();
    assert_registration(next.url(), server.base(), &host_id);
    drop(next);
    assert!(sign_in_with(folder.path(), &script, &endpoints, "code=c&client_id=oaiapp_issued-2").unwrap().plan_usage);
    assert_eq!(store::client(folder.path()).unwrap().client_id.as_deref(), Some("oaiapp_issued-2"));
    assert_eq!(store::read_credentials(folder.path()).unwrap().client_id, "oaiapp_issued-2");
    // A code exchange refused for the same reason (in OpenAI's shape) forgets it too.
    *script.refuse.lock().unwrap() = Some((401, json!({"error": {"code": "invalid_client", "message": "unknown client"}})));
    let e = sign_in_with(folder.path(), &script, &endpoints, "code=c3").unwrap_err();
    assert!(e.to_string().contains("registration"), "{e}");
    assert_eq!(store::client(folder.path()).unwrap().client_id, None);
}

#[test]
fn a_refresh_whose_id_token_fails_its_checks_keeps_the_rotated_refresh_token() {
    let (folder, script, server, endpoints) = signed_in(no_api);
    let rotated = |claims: &Value| {
        json!({"access_token": "access-r", "refresh_token": "refresh-r", "id_token": id_token(claims),
               "token_type": "Bearer", "expires_in": 3600, "scope": SCOPES})
    };
    // A clock more than an hour ahead: the refresh's new id token looks expired. The server has
    // spent refresh-0 all the same: the new tokens are kept, with the last id token.
    let mut stale = claims("oaiapp_issued-1", None);
    stale["exp"] = json!(now() - 3600);
    *script.refuse.lock().unwrap() = Some((200, rotated(&stale)));
    expire(folder.path());
    assert_eq!(access_token(folder.path(), &endpoints, None).unwrap().access_token, "access-r");
    let saved = store::read_credentials(folder.path()).unwrap();
    assert_eq!((saved.refresh_token.as_str(), saved.subject.as_str(), saved.email.as_str()), ("refresh-r", "user-123", "writer@example.com"));
    // The next refresh sends the rotated token, not the spent one.
    *script.refuse.lock().unwrap() = None;
    expire(folder.path());
    access_token(folder.path(), &endpoints, None).unwrap();
    assert_eq!(server.to("/oauth/token").last().and_then(|r| r.field("refresh_token")).as_deref(), Some("refresh-r"));
    // Another account's tokens end the sign-in: neither used nor kept, but revoked.
    let mut other = claims("oaiapp_issued-1", None);
    other["sub"] = json!("someone-else");
    *script.refuse.lock().unwrap() = Some((200, rotated(&other)));
    expire(folder.path());
    let e = access_token(folder.path(), &endpoints, None).unwrap_err();
    assert!(e.to_string().contains("another account"), "{e}");
    assert_eq!(super::account(folder.path()), None);
    eventually("the other account's refresh token is revoked", || {
        server.to("/oauth/revoke").iter().any(|r| r.field("token").as_deref() == Some("refresh-r"))
    });
}

#[test]
fn the_lock_waits_for_its_holder_is_never_taken_over_and_stays() {
    let folder = Folder::new();
    let path = folder.path().join(store::LOCK_FILE);
    // The file is made by the first lock: empty, and only its owner's.
    drop(store::Lock::within(folder.path(), WAIT).unwrap());
    #[cfg(unix)]
    assert_eq!(mode(&path), 0o600);
    // Another process holds it longer than the wait: an error, and the holder keeps it.
    let other = hold_lock(folder.path());
    let Err(e) = store::Lock::within(folder.path(), Duration::from_millis(200)) else { panic!("the lock was taken over") };
    assert!(e.contains("try again"), "{e}");
    assert!(held(&path));
    drop(other);
    // Once it lets go, the lock is taken.
    let lock = store::Lock::within(folder.path(), WAIT).unwrap();
    assert!(held(&path));
    drop(lock);
    // Let go of, the file stays (deleting it would let two holders in), empty.
    assert!(!held(&path));
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
}

#[test]
fn a_sign_out_waits_for_another_processs_refresh_and_revokes_the_token_it_saved() {
    let (folder, _script, server, endpoints) = signed_in(no_api);
    // Another WordCraft process with this settings folder is refreshing: it holds the lock.
    let other = hold_lock(folder.path());
    let (f, e) = (folder.path().to_path_buf(), endpoints.clone());
    let out = std::thread::spawn(move || sign_out_with(&f, &e));
    std::thread::sleep(Duration::from_millis(500));
    assert!(!out.is_finished(), "the sign-out waits for the other process's refresh");
    // Its refresh comes back: it keeps the rotated token, then lets go.
    let mut c = store::read_credentials(folder.path()).unwrap();
    (c.access_token, c.refresh_token) = ("access-9".into(), "refresh-9".into());
    store::write_credentials(folder.path(), &c).unwrap();
    drop(other);
    assert_eq!(out.join().unwrap(), Ok(SignedOut { revoked: true }));
    // The newest refresh token is the one revoked, and nobody stays signed in.
    let revoked: Vec<Option<String>> = server.to("/oauth/revoke").iter().map(|r| r.field("token")).collect();
    assert_eq!(revoked, [Some("refresh-9".to_string())]);
    assert_eq!(super::account(folder.path()), None);
}

#[test]
fn a_refresh_waits_for_another_processs_and_uses_the_token_it_saved() {
    let (folder, _script, server, endpoints) = signed_in(no_api);
    expire(folder.path());
    let other = hold_lock(folder.path());
    let (f, e) = (folder.path().to_path_buf(), endpoints.clone());
    let refreshing = std::thread::spawn(move || access_token(&f, &e, None));
    std::thread::sleep(Duration::from_millis(500));
    assert!(!refreshing.is_finished(), "the refresh waits for the other process's");
    // The other process's refresh saves the rotated tokens, and lets go.
    let mut c = store::read_credentials(folder.path()).unwrap();
    (c.access_token, c.refresh_token, c.expires_at) = ("access-9".into(), "refresh-9".into(), now() + 3600);
    store::write_credentials(folder.path(), &c).unwrap();
    drop(other);
    assert_eq!(refreshing.join().unwrap().unwrap().access_token, "access-9");
    assert_eq!(server.to("/oauth/token").len(), 1, "refresh-0 isn't sent a second time");
}

#[test]
fn a_refresh_that_comes_back_after_a_sign_out_is_revoked_not_saved() {
    let (folder, script, server, endpoints) = signed_in(no_api);
    expire(folder.path());
    *script.slow.lock().unwrap() = Duration::from_secs(1);
    let (f, e) = (folder.path().to_path_buf(), endpoints.clone());
    let refreshing = std::thread::spawn(move || access_token(&f, &e, None));
    eventually("the refresh is sent", || server.to("/oauth/token").len() == 2);
    // Signed out meanwhile, where the lock can't keep it out (a folder without file locks, or
    // by hand).
    store::delete_credentials(folder.path()).unwrap();
    assert_eq!(refreshing.join().unwrap().unwrap_err(), AuthError::NotSignedIn);
    assert!(store::read_credentials(folder.path()).is_none(), "the sign-out stays");
    // The tokens nobody keeps are revoked.
    eventually("the new refresh token is revoked", || server.to("/oauth/revoke").iter().any(|r| r.field("token").as_deref() == Some("refresh-1")));
}

#[test]
fn a_new_sign_in_is_saved_after_a_refresh_in_flight_and_its_tokens_kept() {
    let (folder, script, server, endpoints) = signed_in(no_api);
    // Another WordCraft process is refreshing the last sign-in: it holds the lock.
    let other = hold_lock(folder.path());
    let mut s = SignIn::start(folder.path(), options(&endpoints)).unwrap();
    let url = s.url().to_string();
    *script.nonce.lock().unwrap() = param(&url, "nonce").unwrap();
    let (redirect, state) = (s.redirect_uri().to_string(), param(&url, "state").unwrap());
    let browser = std::thread::spawn(move || callback(&redirect, &format!("code=c2&state={state}")));
    eventually("the code is exchanged", || server.to("/oauth/token").len() == 2);
    assert!(s.wait(Duration::from_millis(300)).is_none(), "the new sign-in waits for the refresh");
    // The other process's refresh comes back: it saves the last sign-in's rotated tokens, then
    // lets go.
    let mut last = store::read_credentials(folder.path()).unwrap();
    (last.access_token, last.refresh_token) = ("access-old".into(), "refresh-old".into());
    store::write_credentials(folder.path(), &last).unwrap();
    drop(other);
    assert!(s.wait(WAIT).unwrap().is_ok());
    assert_eq!(browser.join().unwrap().0, 200);
    // The new sign-in's tokens are the ones kept.
    let saved = store::read_credentials(folder.path()).unwrap();
    assert_eq!((saved.access_token.as_str(), saved.refresh_token.as_str()), ("access-1", "refresh-1"));
}

#[test]
fn signing_out_revokes_the_refresh_token_and_deletes_the_credentials_but_keeps_the_client() {
    let (folder, script, server, endpoints) = signed_in(no_api);
    let host_id = store::client(folder.path()).unwrap().host_id;
    assert_eq!(sign_out_with(folder.path(), &endpoints), Ok(SignedOut { revoked: true }));
    let revoke = &server.to("/oauth/revoke")[0];
    assert_eq!(revoke.field("token").as_deref(), Some("refresh-0"));
    assert_eq!(revoke.field("token_type_hint").as_deref(), Some("refresh_token"));
    assert_eq!(revoke.field("client_id").as_deref(), Some("oaiapp_issued-1"));
    assert_eq!(folder.files(), [CLIENT_FILE, store::LOCK_FILE], "the tokens are gone");
    let client = store::client(folder.path()).unwrap();
    assert_eq!((client.host_id, client.client_id.as_deref()), (host_id, Some("oaiapp_issued-1")));
    assert_eq!(super::account(folder.path()), None);
    // The next sign-in reuses the client, without the last id token as a hint.
    let next = SignIn::start(folder.path(), options(&endpoints)).unwrap();
    assert_eq!(param(next.url(), "client_id").as_deref(), Some("oaiapp_issued-1"));
    assert!(param(next.url(), "id_token_hint").is_none());
    drop(next);
    // Signed out twice: nothing to revoke.
    assert_eq!(sign_out_with(folder.path(), &endpoints), Ok(SignedOut { revoked: true }));
    assert_eq!(server.to("/oauth/revoke").len(), 1);
    // The revocation fails (twice): the tokens are deleted anyway, and the result says so.
    sign_in_with(folder.path(), &script, &endpoints, "code=c").unwrap();
    *script.revoke.lock().unwrap() = 503;
    let mut out = SignOut::start(folder.path(), endpoints.clone()).unwrap();
    assert_eq!(out.wait(WAIT).cloned(), Some(Ok(SignedOut { revoked: false })));
    assert_eq!(server.to("/oauth/revoke").len(), 3);
    assert_eq!(folder.files(), [CLIENT_FILE, store::LOCK_FILE]);
}

#[test]
fn a_damaged_missing_or_huge_file_reads_as_signed_out() {
    let folder = Folder::new();
    let nowhere = Endpoints::local("http://127.0.0.1:9");
    assert_eq!(super::account(folder.path()), None);
    assert!(matches!(readiness(Some(folder.path())), Readiness::SignIn { how } if how.contains("Options › Account")));
    assert!(matches!(readiness(None), Readiness::Unavailable { .. }));
    let junk: [Vec<u8>; 8] = [
        Vec::new(),
        b"{".to_vec(),
        b"null".to_vec(),
        b"[1,2,3]".to_vec(),
        b"{\"version\":1}".to_vec(),
        vec![0xff; 100],
        br#"{"version":99,"clientId":"oaiapp_x","hostId":"","issuer":"","subject":"","refreshToken":"r"}"#.to_vec(),
        [b"{\"version\":1,\"x\":\"".to_vec(), vec![b'a'; 300 << 10], b"\"}".to_vec()].concat(),
    ];
    for bytes in junk {
        std::fs::write(folder.path().join(CREDENTIALS_FILE), &bytes).unwrap();
        assert_eq!(super::account(folder.path()), None);
        assert_eq!(access_token(folder.path(), &nowhere, None).unwrap_err(), AuthError::NotSignedIn);
        assert!(matches!(readiness(Some(folder.path())), Readiness::SignIn { .. }));
    }
    // A damaged client file: a new host id (no client id) replaces it.
    std::fs::write(folder.path().join(CLIENT_FILE), b"{\"hostId\": 7").unwrap();
    let client = store::client(folder.path()).unwrap();
    assert!(client.host_id.starts_with("urn:uuid:") && client.client_id.is_none());
    assert_eq!(store::client(folder.path()).unwrap(), client, "kept");
    // A folder that can't hold the files: an error, not a crash.
    let file = folder.path().join("a-file");
    std::fs::write(&file, b"x").unwrap();
    assert!(matches!(SignIn::start(&file, options(&nowhere)), Err(AuthError::Storage(_))));
    assert_eq!(super::account(&file), None);
}

/// A folder of its own whose key goes to `keychain`, signed in at a fake server.
fn signed_in_with(keychain: Option<Arc<dyn Keychain>>) -> (Folder, Arc<Script>, Server, Endpoints) {
    let folder = Folder::new();
    vault::register(folder.path(), keychain);
    let script = Arc::new(Script::default());
    let server = server(script.clone(), no_api);
    let endpoints = Endpoints::local(server.base());
    sign_in_with(folder.path(), &script, &endpoints, "code=the-code&client_id=oaiapp_issued-1").unwrap();
    (folder, script, server, endpoints)
}

#[test]
fn each_folder_s_key_is_in_the_keychain_and_signing_out_deletes_it_with_the_file() {
    let keychain = Arc::new(vault::Fake::default());
    let (a, script, _server, endpoints) = signed_in_with(Some(keychain.clone()));
    let b = Folder::new();
    vault::register(b.path(), Some(keychain.clone()));
    sign_in_with(b.path(), &script, &endpoints, "code=c&client_id=oaiapp_issued-1").unwrap();
    // One 256-bit key per settings folder, named after it.
    let names: Vec<String> = keychain.entries.lock().unwrap().keys().cloned().collect();
    assert_eq!(names.len(), 2, "{names:?}");
    for name in &names {
        let hash = name.strip_prefix("chatgpt-credentials-key:").unwrap();
        assert!(hash.len() == 16 && hash.chars().all(|c| c.is_ascii_hexdigit()), "{name}");
        assert_eq!(keychain.entries.lock().unwrap()[name].len(), 32);
    }
    assert_eq!(super::account(a.path()).map(|x| x.kept), Some(Kept::Keychain));
    // Signing out of one deletes its sealed file and its key; the other's stay.
    assert_eq!(sign_out_with(a.path(), &endpoints), Ok(SignedOut { revoked: true }));
    assert!(!a.path().join(CREDENTIALS_FILE).exists());
    assert_eq!(keychain.entries.lock().unwrap().len(), 1);
    assert_eq!(super::account(a.path()), None);
    assert!(super::account(b.path()).is_some());
}

#[test]
fn with_no_keychain_a_sign_in_is_kept_for_the_session_only() {
    let broken: Arc<dyn Keychain> = Arc::new(vault::Fake { broken: AtomicBool::new(true), ..vault::Fake::default() });
    for keychain in [Some(broken), None] {
        let (folder, _script, _server, endpoints) = signed_in_with(keychain);
        assert_eq!(super::account(folder.path()).map(|a| a.kept), Some(Kept::Session));
        assert_eq!(readiness(Some(folder.path())), Readiness::Ready);
        // Nothing readable, nor sealed, is on disk.
        assert_eq!(folder.files(), [CLIENT_FILE, store::LOCK_FILE]);
        // A refresh keeps its new tokens in memory too.
        expire(folder.path());
        assert_eq!(access_token(folder.path(), &endpoints, None).unwrap().access_token, "access-1");
        assert_eq!(folder.files(), [CLIENT_FILE, store::LOCK_FILE]);
        // Signing out forgets them.
        assert_eq!(sign_out_with(folder.path(), &endpoints), Ok(SignedOut { revoked: true }));
        assert_eq!(super::account(folder.path()), None);
        assert!(store::read_credentials(folder.path()).is_none());
    }
}

#[test]
fn an_older_plaintext_sign_in_is_sealed_or_kept_in_memory_and_its_file_deleted() {
    let (signed, _script, _server, _endpoints) = signed_in(no_api);
    let old = store::read_credentials(signed.path()).unwrap();
    let plaintext = serde_json::to_vec_pretty(&old).unwrap();
    for working in [true, false] {
        let folder = Folder::new();
        fake_keychain(folder.path(), working);
        std::fs::write(folder.path().join(PLAINTEXT_FILE), &plaintext).unwrap();
        // The window's read doesn't wait for the keychain: a thread of its own moves the file. The
        // sign-in shows once it is kept, a moment before the older file is deleted.
        eventually("the older sign-in is moved", || super::account(folder.path()).is_some() && !opening(folder.path()));
        let kept = if working { Kept::Keychain } else { Kept::Session };
        assert_eq!(super::account(folder.path()).unwrap().kept, kept);
        assert!(!folder.path().join(PLAINTEXT_FILE).exists(), "the plaintext file is deleted");
        assert_eq!(folder.path().join(CREDENTIALS_FILE).exists(), working);
        assert_eq!(store::read_credentials(folder.path()).unwrap().refresh_token, old.refresh_token);
    }
    // One that can't be read is deleted too, and nobody is signed in.
    let folder = Folder::new();
    std::fs::write(folder.path().join(PLAINTEXT_FILE), br#"{"version":1,"refreshToken":"half"#).unwrap();
    assert!(store::read_credentials(folder.path()).is_none());
    assert!(!folder.path().join(PLAINTEXT_FILE).exists());
}

/// The sign-out holds the credentials' lock when it finds the older file, so the file is moved
/// under that lock: taking it again would wait for itself for ever.
#[test]
fn signing_out_right_after_an_upgrade_deletes_the_older_file_and_revokes_its_token() {
    let (signed, _script, server, endpoints) = signed_in(no_api);
    let old = store::read_credentials(signed.path()).unwrap();
    let folder = Folder::new();
    std::fs::write(folder.path().join(PLAINTEXT_FILE), serde_json::to_vec(&old).unwrap()).unwrap();
    let (f, e) = (folder.path().to_path_buf(), endpoints.clone());
    let out = std::thread::spawn(move || sign_out_with(&f, &e));
    eventually("the sign-out ends", || out.is_finished());
    assert_eq!(out.join().unwrap(), Ok(SignedOut { revoked: true }));
    assert!(!folder.path().join(PLAINTEXT_FILE).exists() && !folder.path().join(CREDENTIALS_FILE).exists());
    assert!(server.to("/oauth/revoke").iter().any(|r| r.field("token").as_deref() == Some("refresh-0")));
}

#[test]
fn a_sealed_file_cut_short_garbled_of_another_version_or_key_reads_as_signed_out() {
    let keychain = Arc::new(vault::Fake::default());
    let (folder, script, _server, endpoints) = signed_in_with(Some(keychain.clone()));
    let nowhere = Endpoints::local("http://127.0.0.1:9");
    let path = folder.path().join(CREDENTIALS_FILE);
    let good = std::fs::read(&path).unwrap();
    let envelope: Value = serde_json::from_slice(&good).unwrap();
    assert_eq!(
        (envelope["format"].as_str(), envelope["version"].as_u64(), envelope["algorithm"].as_str()),
        (Some("wordcraft.chatgpt-credentials"), Some(1), Some("AES-256-GCM"))
    );
    let with = |field: &str, value: Value| {
        let mut e = envelope.clone();
        e[field] = value;
        serde_json::to_vec(&e).unwrap()
    };
    let mut flipped = from_b64url(envelope["ciphertext"].as_str().unwrap()).unwrap();
    flipped[0] ^= 1;
    // The same credentials sealed with the same key, bound to another purpose or version.
    let key = keychain.entries.lock().unwrap().values().next().unwrap().clone();
    let plain = serde_json::to_vec(&store::read_credentials(folder.path()).unwrap()).unwrap();
    let reseal = |aad: &[u8]| {
        use ring::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
        let mut data = plain.clone();
        let sealer = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &key).unwrap());
        sealer.seal_in_place_append_tag(Nonce::assume_unique_for_key([7; 12]), Aad::from(aad), &mut data).unwrap();
        let mut e = envelope.clone();
        (e["nonce"], e["ciphertext"]) = (json!(b64url(&[7; 12])), json!(b64url(&data)));
        serde_json::to_vec(&e).unwrap()
    };
    std::fs::write(&path, reseal(b"wordcraft.chatgpt-credentials/1/AES-256-GCM")).unwrap();
    assert!(store::read_credentials(folder.path()).is_some(), "resealed as WordCraft seals, it opens");
    let damaged = [
        ("cut short", good[..good.len() / 2].to_vec()),
        ("empty", Vec::new()),
        ("not JSON", vec![0xff; 64]),
        ("a flipped bit", with("ciphertext", json!(b64url(&flipped)))),
        ("no tag", with("ciphertext", json!(""))),
        ("a later version", with("version", json!(2))),
        ("another algorithm", with("algorithm", json!("AES-128-GCM"))),
        ("another format", with("format", json!("something-else"))),
        ("a short nonce", with("nonce", json!(b64url(&[0; 8])))),
        ("a nonce that isn't base64url", with("nonce", json!("!!!"))),
        ("a field it doesn't know", with("extra", json!(1))),
        ("bound to another version", reseal(b"wordcraft.chatgpt-credentials/2/AES-256-GCM")),
        ("bound to another purpose", reseal(b"wordcraft.something-else/1/AES-256-GCM")),
        ("too big", vec![b' '; usize::try_from(vault::MAX_SEALED).unwrap() + 1]),
    ];
    for (what, bytes) in damaged {
        std::fs::write(&path, &bytes).unwrap();
        assert!(store::read_credentials(folder.path()).is_none(), "{what}");
        assert_eq!(super::account(folder.path()), None, "{what}");
        assert_eq!(access_token(folder.path(), &nowhere, None).unwrap_err(), AuthError::NotSignedIn, "{what}");
    }
    // WordCraft opens again, and the keychain holds another key, or one that isn't a key.
    std::fs::write(&path, &good).unwrap();
    for other in [vec![9; 32], vec![9; 5]] {
        keychain.entries.lock().unwrap().values_mut().for_each(|k| *k = other.clone());
        vault::register(folder.path(), Some(keychain.clone()));
        assert!(store::read_credentials(folder.path()).is_none(), "{other:?}");
        assert_eq!(super::account(folder.path()), None);
    }
    // The next sign-in replaces both.
    sign_in_with(folder.path(), &script, &endpoints, "code=c2").unwrap();
    assert_eq!(super::account(folder.path()).map(|a| a.kept), Some(Kept::Keychain));
    assert_eq!(keychain.entries.lock().unwrap().values().next().map(Vec::len), Some(32));
}

#[test]
fn a_sign_in_another_wordcraft_sealed_with_a_new_key_is_read() {
    let (folder, script, _server, endpoints) = signed_in(no_api);
    assert!(super::account(folder.path()).is_some());
    // Another WordCraft with this settings folder (named another way, so this process keeps
    // two; `Path` reads `dir/.` as `dir`): it signs out, which deletes the key, and in again,
    // with a new key.
    let other = folder.path().join("..").join(folder.path().file_name().unwrap());
    assert_ne!(other, folder.path());
    fake_keychain(&other, true);
    assert_eq!(sign_out_with(&other, &endpoints), Ok(SignedOut { revoked: true }));
    sign_in_with(&other, &script, &endpoints, "code=c2").unwrap();
    // This one still has the old key: it asks the keychain again, and reads the new sign-in.
    eventually("the new sign-in shows", || super::account(folder.path()).is_some());
    assert_eq!(store::read_credentials(folder.path()).unwrap().access_token, "access-1");
}

/// The biggest sign-in the token endpoint is accepted with (each token at `auth::MAX_TOKEN`) seals
/// into a file the next read takes; one too big for the reader is kept in memory rather than
/// written where no read would take it.
#[test]
fn the_biggest_sign_in_accepted_seals_into_a_file_that_reads_back() {
    let (folder, _script, _server, _endpoints) = signed_in_with(Some(Arc::new(vault::Fake::default())));
    let mut c = store::read_credentials(folder.path()).unwrap();
    let token = |n: &str| n.repeat(super::auth::MAX_TOKEN);
    (c.access_token, c.refresh_token, c.id_token) = (token("a"), token("r"), token("i"));
    assert_eq!(store::write_credentials(folder.path(), &c), Ok(Kept::Keychain));
    let back = store::read_credentials(folder.path()).expect("the sealed file reads back");
    assert_eq!((back.access_token, back.refresh_token, back.id_token), (c.access_token.clone(), c.refresh_token.clone(), c.id_token.clone()));

    c.name = Some("n".repeat(1 << 20));
    assert_eq!(store::write_credentials(folder.path(), &c), Ok(Kept::Session));
    assert!(!folder.path().join(CREDENTIALS_FILE).exists());
    assert_eq!(store::read_credentials(folder.path()).and_then(|c| c.name).map(|n| n.len()), Some(1 << 20));
}

#[test]
fn a_sign_in_kept_in_memory_after_the_keychain_fails_leaves_no_older_sealed_one_behind() {
    let keychain = Arc::new(vault::Fake::default());
    let (folder, _script, _server, endpoints) = signed_in_with(Some(keychain.clone()));
    expire(folder.path());
    // The keychain fails mid-session (a keyring locked again, or macOS refusing a rebuilt app):
    // the refresh's new tokens are kept in memory, and the sealed file of the older ones goes.
    keychain.broken.store(true, Ordering::Relaxed);
    assert_eq!(access_token(folder.path(), &endpoints, None).unwrap().access_token, "access-1");
    assert_eq!(super::account(folder.path()).map(|a| a.kept), Some(Kept::Session));
    assert!(!folder.path().join(CREDENTIALS_FILE).exists());
    // WordCraft opens again and the keychain answers: nobody is signed in, rather than the older
    // tokens, whose refresh token ChatGPT may have rotated since.
    keychain.broken.store(false, Ordering::Relaxed);
    vault::register(folder.path(), Some(keychain.clone()));
    assert!(store::read_credentials(folder.path()).is_none());
}

/// A keychain that answers only once it is let go, as one waiting for the person to unlock it
/// (or after three seconds, so a window that waited for it fails the test rather than hangs it).
#[derive(Default)]
struct Hanging {
    inner: vault::Fake,
    hold: AtomicBool,
}

impl Keychain for Hanging {
    fn get(&self, account: &str) -> Result<Option<Vec<u8>>, String> {
        let asked = Instant::now();
        while self.hold.load(Ordering::Relaxed) && asked.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(5));
        }
        self.inner.get(account)
    }

    fn set(&self, account: &str, secret: &[u8]) -> Result<(), String> {
        self.inner.set(account, secret)
    }

    fn delete(&self, account: &str) -> Result<(), String> {
        self.inner.delete(account)
    }
}

#[test]
fn the_window_never_waits_for_a_keychain_that_hangs() {
    let keychain = Arc::new(Hanging::default());
    let (folder, _script, _server, endpoints) = signed_in_with(Some(keychain.clone()));
    // WordCraft opens again, and the keychain waits for the person to unlock it.
    vault::register(folder.path(), Some(keychain.clone()));
    keychain.hold.store(true, Ordering::Relaxed);
    let started = Instant::now();
    assert_eq!(super::account(folder.path()), None);
    assert!(matches!(readiness(Some(folder.path())), Readiness::SignIn { .. }));
    let next = SignIn::start(folder.path(), options(&endpoints)).unwrap();
    assert!(param(next.url(), "id_token_hint").is_none(), "no hint without the keychain");
    drop(next);
    assert!(started.elapsed() < Duration::from_secs(2), "the window waited {:?}", started.elapsed());
    assert!(opening(folder.path()), "a thread of its own asks the keychain");
    // Unlocked: the sign-in shows.
    keychain.hold.store(false, Ordering::Relaxed);
    eventually("the sign-in is read", || super::account(folder.path()).is_some());
    assert!(!opening(folder.path()));
}

#[test]
fn a_sign_in_without_plan_usage_is_kept_and_says_how_to_allow_it() {
    let folder = Folder::new();
    let script = Arc::new(Script::default());
    *script.scope.lock().unwrap() = Some("openid profile email offline_access".into());
    let server = server(script.clone(), no_api);
    let endpoints = Endpoints::local(server.base());
    let account = sign_in_with(folder.path(), &script, &endpoints, "code=c&client_id=oaiapp_issued-1").unwrap();
    assert!(!account.plan_usage);
    assert_eq!(super::account(folder.path()).map(|a| a.plan_usage), Some(false));
    assert!(matches!(readiness(Some(folder.path())), Readiness::SignIn { how } if how.contains("allow")));
    // Asking again with consent grants it.
    *script.scope.lock().unwrap() = None;
    let mut again = SignIn::start(folder.path(), SignInOptions { consent: true, ..options(&endpoints) }).unwrap();
    let url = again.url().to_string();
    *script.nonce.lock().unwrap() = param(&url, "nonce").unwrap();
    let _ = callback(again.redirect_uri(), &format!("code=c&state={}", param(&url, "state").unwrap()));
    assert!(again.wait(WAIT).cloned().unwrap().unwrap().plan_usage);
    assert_eq!(readiness(Some(folder.path())), Readiness::Ready);
}

#[test]
fn id_tokens_are_checked_for_issuer_audience_expiry_and_nonce() {
    let good = claims("oaiapp_x", Some("n1"));
    let check = |c: &Value, nonce: Option<&str>| check_id_token(&id_token(c), ISSUER, "oaiapp_x", nonce);
    let Claims { subject, email, name } = check(&good, Some("n1")).unwrap();
    assert_eq!((subject.as_str(), email.as_str(), name.as_deref()), ("user-123", "writer@example.com", Some("A Writer")));
    let with = |key: &str, value: Value| {
        let mut c = good.clone();
        c[key] = value;
        c
    };
    let without = |key: &str| {
        let mut c = good.clone();
        c.as_object_mut().unwrap().remove(key);
        c
    };
    assert!(check(&with("aud", json!("oaiapp_x")), Some("n1")).is_ok(), "a single audience");
    assert!(check(&without("nonce"), None).is_ok(), "a refresh's id token has no nonce to match");
    assert!(check(&without("sid"), Some("n1")).is_ok());
    assert!(check(&with("exp", json!(now() - 10)), Some("n1")).is_ok(), "a little clock skew");
    for (bad, why) in [
        (with("iss", json!("https://evil.example")), "issuer"),
        (with("aud", json!(["oaiapp_y"])), "audience"),
        (with("aud", json!("oaiapp_y")), "audience"),
        (without("aud"), "no audience"),
        (with("exp", json!(now() - 3600)), "expired"),
        (with("exp", json!("tomorrow")), "exp as text"),
        (without("exp"), "no exp"),
        (without("iat"), "no iat"),
        (without("sub"), "no sub"),
        (with("sub", json!("")), "empty sub"),
        (with("nonce", json!("n2")), "another nonce"),
        (without("nonce"), "no nonce"),
        (with("sid", json!("")), "empty sid"),
        (with("sid", json!(5)), "sid not text"),
    ] {
        assert!(matches!(check(&bad, Some("n1")), Err(AuthError::Failed(e)) if e.contains("can't be trusted")), "{why}");
    }
    let b = |s: &str| b64url(s.as_bytes());
    for token in ["", "a.b", "a.b.c.d", &format!("x.{}.y", b("not json")), &format!("x.{}.y", b("[1]")), "x.!!!.y", &format!("x.{}.y", b("{}"))] {
        assert!(check_id_token(token, ISSUER, "oaiapp_x", None).is_err(), "{token}");
    }
    assert!(valid_client_id("oaiapp_abc-DEF_123") && !valid_client_id("oaiapp_") && !valid_client_id("dynamic_agent_client"));
    assert!(!valid_client_id(&format!("oaiapp_{}", "a".repeat(201))) && !valid_client_id("oaiapp_a b") && !valid_client_id("oaiapp_é"));
}

#[test]
fn the_models_list_keeps_listed_models_in_order() {
    let listed = json!({"models": [
        {"slug": "gpt-a", "display_name": "GPT A", "visibility": "list"},
        {"slug": "hidden", "display_name": "Hidden", "visibility": "hide"},
        {"slug": "gpt-b"},
        {"slug": ""},
        {"slug": "bad slug"},
        {"display_name": "no slug"},
        7,
        {"slug": "gpt-c", "display_name": "C\u{0}\u{7}"},
    ]});
    let names: Vec<(String, String)> = models_in(&listed).into_iter().map(|m| (m.id, m.name)).collect();
    assert_eq!(names, [("gpt-a".into(), "GPT A".into()), ("gpt-b".into(), "gpt-b".into()), ("gpt-c".into(), "C".into())]);
    assert_eq!(models_in(&json!({"data": [{"id": "gpt-x", "object": "model"}]})).iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["gpt-x"]);
    assert!(models_in(&json!("junk")).is_empty() && models_in(&json!({"models": {"a": 1}})).is_empty());
    let many: Vec<Value> = (0..1000).map(|i| json!({"slug": format!("m{i}")})).collect();
    assert_eq!(models_in(&json!({"models": many})).len(), 64);
    // From the server, with the account's token.
    let (folder, _script, server, endpoints) = signed_in(no_api);
    let models = models_with(folder.path(), &endpoints).unwrap();
    assert_eq!(models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["gpt-test", "gpt-test-mini"]);
    assert_eq!(server.to("/v1/models")[0].header("authorization"), Some("Bearer access-0"));
    assert!(matches!(models_with(Folder::new().path(), &endpoints), Err(AuthError::NotSignedIn)));
}

#[test]
fn a_models_list_that_turns_a_fresh_token_away_refreshes_it_once() {
    let (folder, script, server, endpoints) = signed_in(no_api);
    script.turned_away.lock().unwrap().push("access-0".into());
    let models = models_with(folder.path(), &endpoints).unwrap();
    assert_eq!(models[0].id, "gpt-test");
    let sent: Vec<Option<String>> = server.to("/v1/models").iter().map(|r| r.header("authorization").map(str::to_string)).collect();
    assert_eq!(sent, [Some("Bearer access-0".to_string()), Some("Bearer access-1".to_string())]);
    assert_eq!(server.to("/oauth/token").len(), 2, "the exchange, then one refresh");
    // Turned away again after the refresh: one refresh, no loop, and the sign-in stays.
    script.turned_away.lock().unwrap().extend(["access-1".to_string(), "access-2".to_string()]);
    let e = models_with(folder.path(), &endpoints).unwrap_err();
    assert!(matches!(&e, AuthError::Failed(why) if why.contains("didn't accept your sign-in")), "{e:?}");
    assert_eq!(server.to("/oauth/token").len(), 3);
    assert!(super::account(folder.path()).is_some(), "the sign-in stays");
    // Only a refresh that says the sign-in has ended ends it.
    *script.refuse.lock().unwrap() = Some((400, json!({"error": "invalid_grant"})));
    assert_eq!(models_with(folder.path(), &endpoints).unwrap_err(), AuthError::SignInAgain);
    assert_eq!(super::account(folder.path()), None);
}

#[test]
fn the_pane_offers_chatgpt_with_what_it_needs_first() {
    let folder = Folder::new();
    let host = Host { settings: Some(folder.path().to_path_buf()), ..Host::default() };
    let all = crate::offers::offers(&host);
    let chatgpt = all.iter().find(|o| o.id == ID).unwrap();
    assert_eq!(chatgpt.label, "ChatGPT");
    assert!(matches!(&chatgpt.readiness, Readiness::SignIn { how } if how.contains("Plus or Pro")));
    let e = crate::offers::make(ID, &host).err().unwrap();
    assert!(e.starts_with("ChatGPT: Sign in"), "{e}");
    let nowhere = crate::offers::offers(&Host::default());
    assert!(matches!(&nowhere.iter().find(|o| o.id == ID).unwrap().readiness, Readiness::Unavailable { .. }));
}

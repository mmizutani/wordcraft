//! Sign in with ChatGPT against fake servers on this machine: the whole flow, refreshes,
//! sign-out, and junk from the browser, the servers and the disk.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::auth::{Claims, access_token, check_id_token, models_in, valid_client_id};
use super::fake_server::{Reply, Request, Server};
use super::http::{b64url, now, parse_query};
use super::store;
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
    /// What the token endpoint answers instead of tokens.
    pub(super) refuse: Mutex<Option<(u16, Value)>>,
    /// How long the token endpoint takes.
    pub(super) slow: Mutex<Duration>,
    /// What the revocation endpoint answers (200 when 0).
    pub(super) revoke: Mutex<u16>,
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

fn token(script: &Script, r: &Request, n: usize) -> Reply {
    std::thread::sleep(*script.slow.lock().unwrap());
    if let Some((status, body)) = script.refuse.lock().unwrap().clone() {
        return Reply::json(status, &body);
    }
    let client = r.field("client_id").unwrap_or_default();
    let first = r.field("grant_type").as_deref() == Some("authorization_code");
    let nonce = script.nonce.lock().unwrap().clone();
    let scope = script.scope.lock().unwrap().clone().unwrap_or_else(|| SCOPES.to_string());
    Reply::json(
        200,
        &json!({
            "access_token": format!("access-{n}"),
            "refresh_token": format!("refresh-{n}"),
            "id_token": id_token(&claims(&client, first.then_some(nonce.as_str()))),
            "token_type": "Bearer",
            "expires_in": 3600,
            "scope": scope,
        }),
    )
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

/// What a sign-in keeps: the issued client id with the host id, and the tokens, in files only
/// their owner reads, written whole.
fn assert_kept(folder: &Folder, host_id: &str) {
    let client = store::client(folder.path()).unwrap();
    assert_eq!((client.host_id.as_str(), client.client_id.as_deref()), (host_id, Some("oaiapp_issued-1")));
    let saved = store::read_credentials(folder.path()).unwrap();
    assert_eq!((saved.access_token.as_str(), saved.refresh_token.as_str()), ("access-0", "refresh-0"));
    assert_eq!((saved.subject.as_str(), saved.issuer.as_str()), ("user-123", ISSUER));
    assert!(saved.plan_usage() && saved.expires_at > now() + 3000 && saved.id_token.split('.').count() == 3);
    assert_eq!(folder.files(), [CLIENT_FILE, CREDENTIALS_FILE]);
    #[cfg(unix)]
    {
        assert_eq!(mode(&folder.path().join(CREDENTIALS_FILE)), 0o600);
        assert_eq!(mode(&folder.path().join(CLIENT_FILE)), 0o600);
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
    assert_eq!(account, Account { email: "writer@example.com".into(), name: Some("A Writer".into()), plan_usage: true });
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
    *script.refuse.lock().unwrap() = Some((400, json!({"error": "invalid_grant", "error_description": "code the-code was used"})));
    let e = attempt("code=the-code&client_id=oaiapp_issued-1").unwrap_err().to_string();
    assert!(e.contains("start signing in again") && !e.contains("the-code"), "{e}");
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
    assert!(!folder.path().join("chatgpt-credentials.lock").exists(), "the lock is let go");
    // A token the API turned away is refreshed though it hasn't expired; once another caller
    // has, the newer one is used as it is.
    *script.slow.lock().unwrap() = Duration::ZERO;
    assert_eq!(access_token(folder.path(), &endpoints, Some("access-1")).unwrap().access_token, "access-2");
    assert_eq!(access_token(folder.path(), &endpoints, Some("access-1")).unwrap().access_token, "access-2");
    assert_eq!(server.to("/oauth/token").len(), 3);
}

#[test]
fn a_refused_refresh_signs_out_but_trouble_on_the_server_keeps_the_sign_in() {
    let (folder, script, server, endpoints) = signed_in(no_api);
    expire(folder.path());
    // The server's trouble: the sign-in stays, and the error says when to try again.
    *script.refuse.lock().unwrap() = Some((503, json!({"error": "temporarily_unavailable"})));
    let e = access_token(folder.path(), &endpoints, None).unwrap_err();
    assert!(matches!(&e, AuthError::Network(why) if why.contains("temporarily_unavailable")), "{e:?}");
    assert!(super::account(folder.path()).is_some());
    // A configuration problem says so, and keeps the sign-in too.
    *script.refuse.lock().unwrap() = Some((401, json!({"error": "invalid_client"})));
    assert!(access_token(folder.path(), &endpoints, None).unwrap_err().to_string().contains("registration"));
    assert!(super::account(folder.path()).is_some());
    // A refresh token that was used or expired: the person signs in again.
    for code in ["invalid_grant", "refresh_token_reused", "refresh_token_expired"] {
        *script.refuse.lock().unwrap() = Some((400, json!({"error": code, "error_description": "refresh-0 is spent"})));
        let mut c = store::read_credentials(folder.path()).unwrap();
        c.refresh_token = "refresh-0".into();
        store::write_credentials(folder.path(), &c).unwrap();
        let e = access_token(folder.path(), &endpoints, None).unwrap_err();
        assert_eq!(e, AuthError::SignInAgain, "{code}");
        assert!(!e.to_string().contains("refresh-0"));
        assert_eq!(super::account(folder.path()), None);
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
fn signing_out_revokes_the_refresh_token_and_deletes_the_credentials_but_keeps_the_client() {
    let (folder, script, server, endpoints) = signed_in(no_api);
    let host_id = store::client(folder.path()).unwrap().host_id;
    assert_eq!(sign_out_with(folder.path(), &endpoints), Ok(SignedOut { revoked: true }));
    let revoke = &server.to("/oauth/revoke")[0];
    assert_eq!(revoke.field("token").as_deref(), Some("refresh-0"));
    assert_eq!(revoke.field("token_type_hint").as_deref(), Some("refresh_token"));
    assert_eq!(revoke.field("client_id").as_deref(), Some("oaiapp_issued-1"));
    assert_eq!(folder.files(), [CLIENT_FILE], "the tokens are gone");
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
    assert_eq!(folder.files(), [CLIENT_FILE]);
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
fn the_models_list_keeps_listed_models_in_order_and_the_choice_is_kept() {
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
    let names: Vec<(String, String)> = models_in(&listed).into_iter().map(|m| (m.slug, m.name)).collect();
    assert_eq!(names, [("gpt-a".into(), "GPT A".into()), ("gpt-b".into(), "gpt-b".into()), ("gpt-c".into(), "C".into())]);
    assert_eq!(models_in(&json!({"data": [{"id": "gpt-x", "object": "model"}]})), [Model { slug: "gpt-x".into(), name: "gpt-x".into() }]);
    assert!(models_in(&json!("junk")).is_empty() && models_in(&json!({"models": {"a": 1}})).is_empty());
    let many: Vec<Value> = (0..1000).map(|i| json!({"slug": format!("m{i}")})).collect();
    assert_eq!(models_in(&json!({"models": many})).len(), 64);
    // From the server, with the account's token.
    let (folder, _script, server, endpoints) = signed_in(no_api);
    let models = models_with(folder.path(), &endpoints).unwrap();
    assert_eq!(models.iter().map(|m| m.slug.as_str()).collect::<Vec<_>>(), ["gpt-test", "gpt-test-mini"]);
    assert_eq!(server.to("/v1/models")[0].header("authorization"), Some("Bearer access-0"));
    assert!(matches!(models_with(Folder::new().path(), &endpoints), Err(AuthError::NotSignedIn)));
    // The choice.
    assert_eq!(chosen_model(folder.path()), None);
    choose_model(folder.path(), Some(" gpt-test-mini ")).unwrap();
    assert_eq!(chosen_model(folder.path()).as_deref(), Some("gpt-test-mini"));
    assert!(choose_model(folder.path(), Some("not a model\n")).is_err());
    assert!(choose_model(folder.path(), Some(&"m".repeat(200))).is_err());
    assert_eq!(chosen_model(folder.path()).as_deref(), Some("gpt-test-mini"));
    choose_model(folder.path(), None).unwrap();
    assert_eq!(chosen_model(folder.path()), None);
    assert_eq!(store::client(folder.path()).unwrap().client_id.as_deref(), Some("oaiapp_issued-1"), "the client id stays");
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

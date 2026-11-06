//! Sign in with ChatGPT from the window (#19), against a fake sign-in server on this machine:
//! the person's sign-in from Options › Account, Cancel, Sign Out, a sign-in without plan usage,
//! and a script's or a faked click's attempt, which starts nothing.
#![cfg(not(target_arch = "wasm32"))]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::json;
use wordcraft_assist::Readiness;
use wordcraft_assist::chatgpt::{CREDENTIALS_FILE, Endpoints, ID, SCOPES};
use wordcraft_engine::cmd::assist::ChatGptStatus;

use super::{Notice, YOURS, status};
use crate::{Services, WordApp};

const WAIT: Duration = Duration::from_secs(20);

/// A settings folder of its own, removed afterwards.
struct Folder(PathBuf);

impl Folder {
    fn new() -> Folder {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("wordcraft-ui-chatgpt-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Folder(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// What the fake sign-in server does, and what it was asked.
#[derive(Default)]
struct Script {
    /// The nonce the id token carries (the sign-in's).
    nonce: Mutex<String>,
    /// The scopes granted, when not all of them.
    scope: Mutex<Option<String>>,
    /// The paths asked for, in order.
    asked: Mutex<Vec<String>>,
}

/// The fake sign-in server: the token, discovery and revocation endpoints.
struct Server {
    base: String,
    script: Arc<Script>,
    stop: Arc<AtomicBool>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn b64url(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(char::from(A[(n >> (18 - 6 * i) & 63) as usize]));
        }
    }
    out
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

/// A form body's field.
fn field(body: &str, name: &str) -> Option<String> {
    body.split('&').filter_map(|kv| kv.split_once('=')).find(|(k, _)| *k == name).map(|(_, v)| v.replace("%20", " ").replace('+', " "))
}

/// An address's query parameter (the sign-in's are URL-safe as they are).
fn param(url: &str, name: &str) -> Option<String> {
    let query = url.split_once('?').map_or("", |(_, q)| q);
    query.split('&').filter_map(|kv| kv.split_once('=')).find(|(k, _)| *k == name).map(|(_, v)| v.to_string())
}

fn answer(stream: &mut TcpStream, status: u16, body: &str) {
    let head = format!("HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
}

/// One request: its method, path and body.
fn read_request(stream: &mut TcpStream) -> Option<(String, String, String)> {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    let head_end = loop {
        let n = stream.read(&mut buf).ok()?;
        if n == 0 {
            return None;
        }
        data.extend_from_slice(&buf[..n]);
        if let Some(i) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&data[..head_end]).to_string();
    let length = head
        .lines()
        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0)))
        .unwrap_or(0);
    while data.len() < head_end + length {
        let n = stream.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
    }
    let mut first = head.lines().next()?.split(' ');
    let (method, target) = (first.next()?.to_string(), first.next()?.to_string());
    let path = target.split('?').next().unwrap_or("").to_string();
    Some((method, path, String::from_utf8_lossy(&data[head_end..]).to_string()))
}

fn serve() -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let script = Arc::new(Script::default());
    let stop = Arc::new(AtomicBool::new(false));
    let (script2, stop2, base2) = (script.clone(), stop.clone(), base.clone());
    std::thread::spawn(move || {
        let mut n = 0;
        while !stop2.load(Ordering::Relaxed) {
            let Ok((mut stream, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            };
            let _ = stream.set_nonblocking(false);
            let Some((method, path, body)) = read_request(&mut stream) else { continue };
            script2.asked.lock().unwrap().push(path.clone());
            n += 1;
            match (method.as_str(), path.as_str()) {
                ("POST", "/oauth/token") => {
                    let client = field(&body, "client_id").unwrap_or_default();
                    let mut claims = json!({
                        "iss": "https://auth.openai.com", "aud": [client], "sub": "user-123", "exp": now() + 3600, "iat": now(),
                        "email": "writer@example.com", "name": "A Writer", "sid": "session-1",
                    });
                    if field(&body, "grant_type").as_deref() == Some("authorization_code") {
                        claims["nonce"] = json!(script2.nonce.lock().unwrap().clone());
                    }
                    let id_token =
                        format!("{}.{}.{}", b64url(br#"{"alg":"RS256","typ":"JWT"}"#), b64url(claims.to_string().as_bytes()), b64url(b"sig"));
                    let scope = script2.scope.lock().unwrap().clone().unwrap_or_else(|| SCOPES.to_string());
                    let tokens = json!({
                        "access_token": format!("access-secret-{n}"), "refresh_token": format!("refresh-secret-{n}"), "id_token": id_token,
                        "token_type": "Bearer", "expires_in": 3600, "scope": scope,
                    });
                    answer(&mut stream, 200, &tokens.to_string());
                }
                ("GET", "/.well-known/openid-configuration") => answer(
                    &mut stream,
                    200,
                    &json!({"issuer": "https://auth.openai.com", "revocation_endpoint": format!("{base2}/oauth/revoke")}).to_string(),
                ),
                ("POST", "/oauth/revoke") => answer(&mut stream, 200, ""),
                _ => answer(&mut stream, 404, "{}"),
            }
        }
    });
    Server { base, script, stop }
}

/// A window with `folder` as its settings folder, signing in at `server`.
fn window(folder: &Folder, server: &Server) -> WordApp {
    let services = Services { settings_dir: Some(folder.path().to_path_buf()), ..Services::default() };
    let mut a = WordApp::new(wordcraft_engine::Session::new(wordcraft_doc::Document::from_text("one two three")), services);
    a.assist.chatgpt.endpoints = Endpoints::local(&server.base);
    a.assist.chatgpt.port = 0;
    crate::assist::tick(&mut a);
    a
}

/// Ticks until `done` holds.
fn until(a: &mut WordApp, what: &str, done: impl Fn(&WordApp) -> bool) {
    let end = Instant::now() + WAIT;
    while !done(a) {
        assert!(Instant::now() < end, "never: {what}");
        crate::assist::tick(a);
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The browser comes back from ChatGPT to the sign-in's port with a code (and, the first time,
/// the client id WordCraft was registered with).
fn browser_returns(a: &WordApp, server: &Server) {
    let url = a.canvas.open_url.clone().expect("the browser was opened");
    *server.script.nonce.lock().unwrap() = param(&url, "nonce").unwrap();
    let state = param(&url, "state").unwrap();
    let redirect = a.assist.chatgpt.sign_in.as_ref().unwrap().redirect_uri().to_string();
    let (host, path) = redirect.strip_prefix("http://").unwrap().split_once('/').unwrap();
    let mut s = TcpStream::connect(host).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let query = format!("code=the-code&client_id=oaiapp_issued-1&state={state}");
    s.write_all(format!("GET /{path}?{query} HTTP/1.1\r\nHost: {host}\r\n\r\n").as_bytes()).unwrap();
    let mut page = Vec::new();
    let _ = s.read_to_end(&mut page);
}

fn chatgpt_offer(a: &WordApp) -> Readiness {
    a.assist.offers.iter().find(|o| o.id == ID).map(|o| o.readiness.clone()).expect("ChatGPT is offered")
}

/// Sign in as the person does, from Options › Account.
fn sign_in(a: &mut WordApp, server: &Server) {
    assert_eq!(a.run("assist.chatgpt.signIn", json!({})).unwrap(), json!({"signingIn": true}));
    browser_returns(a, server);
    until(a, "signed in", |a| status(a) == ChatGptStatus::SignedIn);
}

#[test]
fn the_person_signs_in_and_chatgpt_is_ready_at_once_but_a_script_can_t() {
    let (folder, server) = (Folder::new(), serve());
    let mut a = window(&folder, &server);
    assert_eq!(status(&a), ChatGptStatus::SignedOut);
    assert!(matches!(chatgpt_offer(&a), Readiness::SignIn { .. }));
    // A script or an agent can't start one: nothing listens, no browser opens.
    let e = a.execute("assist.chatgpt.signIn", json!({})).unwrap_err();
    assert!(e.contains("Only you"), "{e}");
    assert!(a.assist.chatgpt.sign_in.is_none() && a.canvas.open_url.is_none());
    // The person's: the browser opens at ChatGPT's address, which the answer never holds.
    let r = a.run("assist.chatgpt.signIn", json!({})).unwrap();
    assert_eq!(r, json!({"signingIn": true}));
    let url = a.canvas.open_url.clone().unwrap();
    assert!(url.starts_with(&format!("{}/authorize?", server.base)), "{url}");
    crate::assist::tick(&mut a);
    assert_eq!(a.session.assistant.chatgpt, ChatGptStatus::SigningIn);
    assert_eq!(a.session.why_disabled("assist.chatgpt.signOut"), Some("a ChatGPT sign-in is waiting for your browser: cancel it first"));
    // Signing in again while it waits opens the browser at the same address.
    a.canvas.open_url = None;
    a.run("assist.chatgpt.signIn", json!({})).unwrap();
    assert_eq!(a.canvas.open_url.as_deref(), Some(url.as_str()));
    browser_returns(&a, &server);
    until(&mut a, "signed in", |a| status(a) == ChatGptStatus::SignedIn);
    let account = super::account(&a).cloned().unwrap();
    assert_eq!((account.email.as_str(), account.plan_usage), ("writer@example.com", true));
    assert_eq!(a.assist.chatgpt.notice, Some(Notice::SignedIn));
    // ChatGPT is ready without a restart, and new conversations can use it.
    assert_eq!(chatgpt_offer(&a), Readiness::Ready);
    assert!(a.session.assistant.ready);
    assert_eq!(a.session.why_disabled("assist.chatgpt.signOut"), None);
    // The tokens stay in the settings folder: neither the window's answers nor its state hold one.
    let listed = a.execute("assist.backend", json!({})).unwrap().to_string();
    assert!(!listed.contains("secret"), "{listed}");
    assert!(!format!("{:?} {:?}", a.assist.chatgpt.account, a.assist.chatgpt.sign_in.as_ref().map(|s| format!("{s:?}"))).contains("secret"));
    assert!(std::fs::read_to_string(folder.path().join(CREDENTIALS_FILE)).unwrap().contains("refresh-secret"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(folder.path().join(CREDENTIALS_FILE)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn cancel_stops_the_sign_in_and_closes_its_port() {
    let (folder, server) = (Folder::new(), serve());
    let mut a = window(&folder, &server);
    a.run("assist.chatgpt.signIn", json!({})).unwrap();
    let redirect = a.assist.chatgpt.sign_in.as_ref().unwrap().redirect_uri().to_string();
    let host = redirect.strip_prefix("http://").unwrap().split_once('/').unwrap().0.to_string();
    // Only the person cancels it.
    let e = a.execute("assist.chatgpt.cancelSignIn", json!({})).unwrap_err();
    assert!(e.contains("Only you"), "{e}");
    assert!(a.assist.chatgpt.sign_in.is_some());
    assert_eq!(a.run("assist.chatgpt.cancelSignIn", json!({})).unwrap(), json!({"cancelled": true}));
    crate::assist::tick(&mut a);
    assert_eq!(a.session.assistant.chatgpt, ChatGptStatus::SignedOut);
    assert_eq!(a.session.why_disabled("assist.chatgpt.cancelSignIn"), Some("no ChatGPT sign-in is waiting for your browser"));
    // Nothing listens for the browser any more.
    let end = Instant::now() + WAIT;
    while TcpStream::connect(&host).is_ok() {
        assert!(Instant::now() < end, "the sign-in's port stays open");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!folder.path().join(CREDENTIALS_FILE).exists());
    assert!(matches!(chatgpt_offer(&a), Readiness::SignIn { .. }));
}

#[test]
fn signing_out_deletes_the_sign_in_and_revokes_it() {
    let (folder, server) = (Folder::new(), serve());
    let mut a = window(&folder, &server);
    sign_in(&mut a, &server);
    let e = a.execute("assist.chatgpt.signOut", json!({})).unwrap_err();
    assert!(e.contains("Only you"), "{e}");
    assert!(folder.path().join(CREDENTIALS_FILE).exists(), "a script signed nobody out");
    assert_eq!(a.run("assist.chatgpt.signOut", json!({})).unwrap(), json!({"signingOut": true}));
    until(&mut a, "signed out", |a| status(a) == ChatGptStatus::SignedOut);
    assert!(!folder.path().join(CREDENTIALS_FILE).exists());
    assert_eq!(a.assist.chatgpt.notice, Some(Notice::SignedOut { revoked: true }));
    assert!(server.script.asked.lock().unwrap().iter().any(|p| p == "/oauth/revoke"), "ChatGPT was asked to end it");
    assert!(matches!(chatgpt_offer(&a), Readiness::SignIn { .. }));
    assert_eq!(a.session.why_disabled("assist.chatgpt.signOut"), Some("you aren't signed in to ChatGPT"));
}

#[test]
fn a_sign_in_without_plan_usage_says_so_and_the_next_one_asks_again() {
    let (folder, server) = (Folder::new(), serve());
    *server.script.scope.lock().unwrap() = Some("openid profile email offline_access resource.invoke".into());
    let mut a = window(&folder, &server);
    sign_in(&mut a, &server);
    assert!(!super::account(&a).unwrap().plan_usage);
    assert!(matches!(chatgpt_offer(&a), Readiness::SignIn { how } if how.contains("plan usage")));
    // The page says so, with the way to fix it.
    a.run("file.options", json!({"pane": "account"})).unwrap();
    let mut h = harness(a);
    use egui_kittest::kittest::Queryable;
    assert!(h.query_by_label_contains("plan usage isn't allowed").is_some());
    h.get_by_label("Sign In Again").click();
    h.run_steps(3);
    // ChatGPT asks again whether WordCraft may use the plan.
    let url = h.state().canvas.open_url.clone();
    let url = url.or_else(|| h.state().assist.chatgpt.sign_in.as_ref().map(|s| s.url().to_string())).unwrap();
    assert_eq!(param(&url, "prompt").as_deref(), Some("consent"), "{url}");
}

/// The window drawn by kittest, at File's Options.
fn harness(a: WordApp) -> egui_kittest::Harness<'static, WordApp> {
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1440.0, 1000.0)).build_ui_state(
        |ui, app: &mut WordApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        a,
    );
    h.run_steps(4);
    h
}

/// One 1440 by 900 frame as the desktop runs it, with the person's own `events`; control input
/// goes in through the raw-input hook. Returns the accessibility tree.
fn frame(a: &mut WordApp, ctx: &egui::Context, events: Vec<egui::Event>) -> egui::accesskit::TreeUpdate {
    let time = ctx.input(|i| i.time) + 0.05;
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1440.0, 900.0));
    let mut raw = egui::RawInput { focused: true, time: Some(time), screen_rect: Some(screen), events, ..Default::default() };
    a.raw_input_hook(ctx, &mut raw);
    let mut out = ctx.run_ui(raw, |ui| {
        a.logic(ui.ctx());
        a.ui(ui);
    });
    out.textures_delta.clear();
    out.platform_output.accesskit_update.take().unwrap_or_else(|| egui::accesskit::TreeUpdate {
        nodes: Vec::new(),
        tree: None,
        tree_id: egui::accesskit::TreeId::ROOT,
        focus: egui::accesskit::NodeId(0),
    })
}

/// The centre of the first widget whose name starts with `label`.
fn spot(tree: &egui::accesskit::TreeUpdate, label: &str) -> Option<(f64, f64)> {
    let (_, node) = tree.nodes.iter().find(|(_, n)| n.label().is_some_and(|l| l.starts_with(label)))?;
    let b = node.bounds()?;
    Some(((b.x0 + b.x1) / 2.0, (b.y0 + b.y1) / 2.0))
}

/// MCP's `click` on Continue with ChatGPT, in Options › Account and in the Assistant pane,
/// starts no sign-in and says why; the person's own click on the same button does.
#[test]
fn a_faked_click_starts_no_sign_in() {
    let (folder, server) = (Folder::new(), serve());
    for place in ["options", "pane"] {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx, crate::theme::Appearance::Light);
        ctx.run_ui(egui::RawInput::default(), |_| {}).drop_without_applying_deltas();
        ctx.enable_accesskit();
        let mut a = window(&folder, &server);
        if place == "options" {
            a.run("file.options", json!({"pane": "account"})).unwrap();
        } else {
            a.run("assist.backend", json!({"id": ID})).unwrap();
            a.run("assist.pane", json!({"value": true})).unwrap();
        }
        let mut tree = frame(&mut a, &ctx, Vec::new());
        for _ in 0..4 {
            tree = frame(&mut a, &ctx, Vec::new());
        }
        let (x, y) = spot(&tree, "Continue with ChatGPT").unwrap_or_else(|| panic!("{place}: the button is there"));
        let (req, _rx) = crate::control::ControlRequest::new("ui.click", json!({"x": x, "y": y}));
        let crate::control::Outcome::Done(_) = crate::control::handle(&mut a, &ctx, &req) else { panic!("ui.click replies at once") };
        for _ in 0..200 {
            frame(&mut a, &ctx, Vec::new());
            if a.synthetic.is_empty() && !a.synthetic_pending {
                break;
            }
        }
        frame(&mut a, &ctx, Vec::new());
        assert!(a.assist.chatgpt.sign_in.is_none() && a.canvas.open_url.is_none(), "{place}: a faked click started a sign-in");
        assert_eq!(a.assist.chatgpt.notice, Some(Notice::SignInFailed(YOURS.into())), "{place}");
        // The person's own click.
        let pos = egui::pos2(x as f32, y as f32);
        frame(&mut a, &ctx, vec![egui::Event::PointerMoved(pos)]);
        for pressed in [true, false] {
            frame(
                &mut a,
                &ctx,
                vec![egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }],
            );
        }
        assert!(a.assist.chatgpt.sign_in.is_some(), "{place}: the person's click starts it");
        a.run("assist.chatgpt.cancelSignIn", json!({})).unwrap();
    }
}

/// The web app (#26) has no sign-in: the commands are greyed out, saying why.
#[test]
fn the_web_app_has_no_sign_in() {
    let (folder, server) = (Folder::new(), serve());
    let mut a = window(&folder, &server);
    a.assist_as_web();
    for id in ["assist.chatgpt.signIn", "assist.chatgpt.signOut"] {
        let e = a.run(id, json!({})).unwrap_err();
        assert!(e.contains("desktop app"), "{id}: {e}");
    }
    assert!(a.assist.chatgpt.sign_in.is_none());
    // File › Options › Account says where to sign in.
    a.run("file.options", json!({"pane": "account"})).unwrap();
    let h = harness(a);
    use egui_kittest::kittest::Queryable;
    assert!(h.query_by_label_contains("takes the WordCraft desktop app").is_some());
    assert!(h.query_by_label("Continue with ChatGPT").is_none());
}

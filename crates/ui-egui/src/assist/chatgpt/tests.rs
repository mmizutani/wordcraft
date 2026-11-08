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

use super::{FINISHING, Notice, YOURS, status};
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
    /// The token endpoint waits while this is set (a code exchange that takes a while).
    hold_token: AtomicBool,
    /// What the models list answers: its status and body.
    models: Mutex<Option<(u16, String)>>,
    /// The models list waits while this is set.
    hold_models: AtomicBool,
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
                    while script2.hold_token.load(Ordering::Relaxed) && !stop2.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(5));
                    }
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
                ("GET", "/v1/models") => {
                    while script2.hold_models.load(Ordering::Relaxed) && !stop2.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    let (status, body) = script2.models.lock().unwrap().clone().unwrap_or((404, "{}".into()));
                    answer(&mut stream, status, &body);
                }
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
    browser_answers(a, "code=the-code&client_id=oaiapp_issued-1");
}

/// The browser comes back to the sign-in's port with `query` (its `state` added); the page it
/// shows.
fn browser_answers(a: &WordApp, query: &str) -> String {
    let (redirect, state) = callback(a);
    answer_at(&redirect, &format!("{query}&state={state}"))
}

/// Where the sign-in waits for the browser, and the state it expects.
fn callback(a: &WordApp) -> (String, String) {
    let url = a.canvas.open_url.clone().expect("the browser was opened");
    let state = param(&url, "state").unwrap();
    (a.assist.chatgpt.sign_in.as_ref().unwrap().redirect_uri().to_string(), state)
}

/// The browser asks `redirect` with `query`: the page it shows.
fn answer_at(redirect: &str, query: &str) -> String {
    let (host, path) = redirect.strip_prefix("http://").unwrap().split_once('/').unwrap();
    let mut s = TcpStream::connect(host).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(format!("GET /{path}?{query} HTTP/1.1\r\nHost: {host}\r\n\r\n").as_bytes()).unwrap();
    let mut page = Vec::new();
    let _ = s.read_to_end(&mut page);
    String::from_utf8_lossy(&page).to_string()
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

/// A macro an agent wrote signs nobody in or out when the person runs it, and an agent can't give
/// Sign Out a key the person would press (#19): both are someone else's arrangement.
#[test]
fn a_macro_or_a_key_an_agent_set_up_signs_nobody_in_or_out() {
    let (folder, server) = (Folder::new(), serve());
    let mut a = window(&folder, &server);
    // Signed out: a sign-in step opens no browser.
    a.execute("tools.macros", json!({"define": {"name": "Start", "steps": [{"command": "assist.chatgpt.signIn"}]}})).unwrap();
    let _ = a.run("tools.macros", json!({"run": "Start"}));
    assert!(a.assist.chatgpt.sign_in.is_none() && a.canvas.open_url.is_none(), "a macro step started a sign-in");
    sign_in(&mut a, &server);
    // An agent (engine.execute: control channel, MCP) writes a macro and tries to bind a key.
    a.execute("tools.macros", json!({"define": {"name": "Tidy headings", "steps": [{"command": "assist.chatgpt.signOut"}]}})).unwrap();
    assert!(a.execute("tools.customizeKeyboard", json!({"assign": {"command": "assist.chatgpt.signOut", "key": "Mod+S"}})).is_err());
    // The person runs the macro from View › Macros.
    let _ = a.run("tools.macros", json!({"run": "Tidy headings"}));
    for _ in 0..20 {
        crate::assist::tick(&mut a);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(status(&a), ChatGptStatus::SignedIn, "a macro step started a sign-out");
    assert!(folder.path().join(CREDENTIALS_FILE).exists(), "the sign-in is kept");
    assert!(!server.script.asked.lock().unwrap().iter().any(|p| p == "/oauth/revoke"), "nothing was revoked");
    // The person's own Sign Out still works.
    a.run("assist.chatgpt.signOut", json!({})).unwrap();
    until(&mut a, "signed out", |a| status(a) == ChatGptStatus::SignedOut);
}

/// Cancel while the browser's answer is being finished (the code is exchanged): the sign-in
/// that gets saved anyway shows at once, not only at the next re-read.
#[test]
fn cancel_while_the_code_is_exchanged_shows_the_sign_in_it_kept() {
    let (folder, server) = (Folder::new(), serve());
    let mut a = window(&folder, &server);
    // No re-read of the settings folder: only the attempt's own end can show the account.
    a.assist.chatgpt.read_at = Some(f64::INFINITY);
    a.run("assist.chatgpt.signIn", json!({})).unwrap();
    server.script.hold_token.store(true, Ordering::Relaxed);
    let url = a.canvas.open_url.clone().unwrap();
    *server.script.nonce.lock().unwrap() = param(&url, "nonce").unwrap();
    let (redirect, state) = callback(&a);
    let browser = std::thread::spawn(move || answer_at(&redirect, &format!("code=the-code&client_id=oaiapp_issued-1&state={state}")));
    let end = Instant::now() + WAIT;
    while !server.script.asked.lock().unwrap().iter().any(|p| p == "/oauth/token") {
        assert!(Instant::now() < end, "the code was never exchanged");
        std::thread::sleep(Duration::from_millis(10));
    }
    crate::assist::tick(&mut a);
    assert_eq!(status(&a), ChatGptStatus::SigningIn, "the window still waits for the browser");
    // The person cancels now; the exchange finishes anyway and keeps the sign-in.
    assert_eq!(a.run("assist.chatgpt.cancelSignIn", json!({})).unwrap(), json!({"cancelled": true}));
    assert_eq!(a.run("assist.chatgpt.signIn", json!({})).unwrap_err(), FINISHING);
    assert!(a.assist.chatgpt.sign_in.is_none());
    server.script.hold_token.store(false, Ordering::Relaxed);
    let _page = browser.join().unwrap();
    until(&mut a, "the kept sign-in shows", |a| status(a) == ChatGptStatus::SignedIn);
    assert!(folder.path().join(CREDENTIALS_FILE).exists());
    assert_eq!(super::account(&a).map(|x| x.email.as_str()), Some("writer@example.com"));
    assert_eq!(a.assist.chatgpt.notice, Some(Notice::SignedIn));
    assert_eq!(chatgpt_offer(&a), Readiness::Ready);
    assert!(a.assist.chatgpt.cancelled.is_none());
}

/// Signed in, the person signs in again and cancels while the code is exchanged: until that
/// sign-in has saved what the browser sent, Sign Out and a new sign-in wait (either would race
/// it: a sign-out it undoes, or a second sign-in whose Cancel loses track of it); then the kept
/// sign-in shows, and Sign Out works.
#[test]
fn a_cancelled_sign_in_still_finishing_holds_back_sign_out_and_a_new_sign_in() {
    let (folder, server) = (Folder::new(), serve());
    let mut a = window(&folder, &server);
    sign_in(&mut a, &server);
    a.assist.chatgpt.read_at = Some(f64::INFINITY);
    let tokens = |server: &Server| server.script.asked.lock().unwrap().iter().filter(|p| *p == "/oauth/token").count();
    let before = tokens(&server);
    a.canvas.open_url = None;
    a.run("assist.chatgpt.signIn", json!({})).unwrap();
    server.script.hold_token.store(true, Ordering::Relaxed);
    let url = a.canvas.open_url.clone().unwrap();
    *server.script.nonce.lock().unwrap() = param(&url, "nonce").unwrap();
    let (redirect, state) = callback(&a);
    let browser = std::thread::spawn(move || answer_at(&redirect, &format!("code=the-code&client_id=oaiapp_issued-1&state={state}")));
    let end = Instant::now() + WAIT;
    while tokens(&server) == before {
        assert!(Instant::now() < end, "the code was never exchanged");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(a.run("assist.chatgpt.cancelSignIn", json!({})).unwrap(), json!({"cancelled": true}));
    assert_eq!(status(&a), ChatGptStatus::SignedIn, "still signed in as before");
    assert_eq!(a.run("assist.chatgpt.signOut", json!({})).unwrap_err(), FINISHING);
    assert!(a.assist.chatgpt.sign_out.is_none());
    assert_eq!(a.run("assist.chatgpt.signIn", json!({})).unwrap_err(), FINISHING);
    assert!(a.assist.chatgpt.sign_in.is_none());
    server.script.hold_token.store(false, Ordering::Relaxed);
    let _page = browser.join().unwrap();
    until(&mut a, "the kept sign-in shows", |a| a.assist.chatgpt.cancelled.is_none());
    assert_eq!(a.assist.chatgpt.notice, Some(Notice::SignedIn));
    assert!(folder.path().join(CREDENTIALS_FILE).exists());
    assert_eq!(a.run("assist.chatgpt.signOut", json!({})).unwrap(), json!({"signingOut": true}));
    until(&mut a, "signed out", |a| status(a) == ChatGptStatus::SignedOut);
    assert!(!folder.path().join(CREDENTIALS_FILE).exists());
}

/// An agent can't put signing in or out on the Quick Access Toolbar, where the person's Alt, 1
/// (or a click) would run it as theirs; one an older settings file put there shows no button, so
/// Alt, 1 runs nothing.
#[test]
fn an_agent_puts_no_sign_in_or_out_on_the_quick_access_toolbar() {
    let (folder, server) = (Folder::new(), serve());
    let mut a = window(&folder, &server);
    sign_in(&mut a, &server);
    let e = a.execute("tools.customizeRibbon", json!({"qatAdd": "assist.chatgpt.signOut"})).unwrap_err();
    assert!(e.contains("only you run it"), "{e}");
    a.execute("tools.customizeRibbon", json!({"layout": {"qat": ["assist.chatgpt.signOut", "file.save"]}})).unwrap();
    assert_eq!(a.session.ribbon.qat(), ["file.save"]);
    a.session.ribbon.qat = Some(vec!["assist.chatgpt.signOut".into()]);
    crate::keytips::activate(&mut a);
    crate::keytips::handle_key(&mut a, egui::Key::Num1);
    crate::assist::tick(&mut a);
    assert!(a.assist.chatgpt.sign_out.is_none(), "the person's Alt, 1 signed them out");
    assert_eq!(status(&a), ChatGptStatus::SignedIn);
    assert!(folder.path().join(CREDENTIALS_FILE).exists());
}

/// A sign-in that ends elsewhere (another WordCraft signs out, or ChatGPT ends it) drops the
/// last "You're signed in" notice: the page doesn't say so under Continue with ChatGPT.
#[test]
fn a_sign_in_ended_elsewhere_drops_the_last_notice() {
    let (folder, server) = (Folder::new(), serve());
    let mut a = window(&folder, &server);
    sign_in(&mut a, &server);
    assert_eq!(a.assist.chatgpt.notice, Some(Notice::SignedIn));
    std::fs::remove_file(folder.path().join(CREDENTIALS_FILE)).unwrap();
    // Ten seconds pass: the next frame reads the settings folder again.
    a.assist.chatgpt.read_at = Some(crate::clock::now_ms() - 11_000.0);
    crate::assist::tick(&mut a);
    assert_eq!(status(&a), ChatGptStatus::SignedOut);
    assert_eq!(a.assist.chatgpt.notice, None);
    a.run("file.options", json!({"pane": "account"})).unwrap();
    let h = harness(a);
    use egui_kittest::kittest::Queryable;
    assert!(h.query_by_label("Continue with ChatGPT").is_some());
    assert!(h.query_by_label_contains("You're signed in to ChatGPT").is_none());
}

/// And the reverse: a sign-in elsewhere drops the "signed out" notice under Signed in as ….
#[test]
fn a_sign_in_elsewhere_drops_the_signed_out_notice() {
    let (folder, server) = (Folder::new(), serve());
    let mut a = window(&folder, &server);
    sign_in(&mut a, &server);
    let kept = std::fs::read(folder.path().join(CREDENTIALS_FILE)).unwrap();
    a.run("assist.chatgpt.signOut", json!({})).unwrap();
    until(&mut a, "signed out", |a| status(a) == ChatGptStatus::SignedOut);
    assert!(matches!(a.assist.chatgpt.notice, Some(Notice::SignedOut { .. })));
    // Another WordCraft signs in: the credentials file is back.
    std::fs::write(folder.path().join(CREDENTIALS_FILE), &kept).unwrap();
    a.assist.chatgpt.read_at = Some(crate::clock::now_ms() - 11_000.0);
    crate::assist::tick(&mut a);
    assert_eq!(status(&a), ChatGptStatus::SignedIn);
    assert_eq!(a.assist.chatgpt.notice, None, "no 'signed out' line under Signed in as …");
}

/// File › Options › Account signed in with plan usage: who, the usage link, and the person's own
/// Sign Out click, which deletes the sign-in and revokes it.
#[test]
fn the_account_page_shows_who_is_signed_in_and_its_sign_out_button_works() {
    let (folder, server) = (Folder::new(), serve());
    let mut a = window(&folder, &server);
    sign_in(&mut a, &server);
    a.run("file.options", json!({"pane": "account"})).unwrap();
    let mut h = harness(a);
    use egui_kittest::kittest::Queryable;
    assert!(h.query_by_label("Signed in as A Writer (writer@example.com)").is_some());
    assert!(h.query_by_label("See your plan's usage").is_some());
    assert!(h.query_by_label_contains("plan usage isn't allowed").is_none());
    assert!(h.query_by_label("Sign In Again").is_none());
    h.get_by_label("Sign Out").click();
    let end = Instant::now() + WAIT;
    while status(h.state()) != ChatGptStatus::SignedOut {
        assert!(Instant::now() < end, "the Sign Out button never signed out");
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!folder.path().join(CREDENTIALS_FILE).exists());
    assert_eq!(h.state().assist.chatgpt.notice, Some(Notice::SignedOut { revoked: true }));
    assert!(server.script.asked.lock().unwrap().iter().any(|p| p == "/oauth/revoke"));
}

/// Declining plan usage in the browser says why, and the next sign-in asks for consent again.
/// (Only WordCraft's client id, which the first sign-in registers, can be asked that: a first
/// sign-in shows ChatGPT's consent anyway.)
#[test]
fn a_declined_sign_in_asks_for_consent_the_next_time() {
    let (folder, server) = (Folder::new(), serve());
    let mut a = window(&folder, &server);
    // Signed in once, so WordCraft has its client id, and signed out: its client id stays.
    sign_in(&mut a, &server);
    a.run("assist.chatgpt.signOut", json!({})).unwrap();
    until(&mut a, "signed out", |a| status(a) == ChatGptStatus::SignedOut);
    a.canvas.open_url = None;
    a.run("assist.chatgpt.signIn", json!({})).unwrap();
    assert!(param(a.canvas.open_url.as_deref().unwrap(), "prompt").is_none());
    browser_answers(&a, "error=access_denied&error_description=The+user+declined");
    until(&mut a, "the decline ended the sign-in", |a| a.assist.chatgpt.sign_in.is_none());
    assert_eq!(status(&a), ChatGptStatus::SignedOut);
    assert_eq!(a.assist.chatgpt.notice, Some(Notice::SignInFailed(wordcraft_assist::chatgpt::AuthError::Denied.to_string())));
    assert!(!folder.path().join(CREDENTIALS_FILE).exists());
    a.canvas.open_url = None;
    a.run("assist.chatgpt.signIn", json!({})).unwrap();
    let url = a.canvas.open_url.clone().unwrap();
    assert_eq!(param(&url, "prompt").as_deref(), Some("consent"), "{url}");
    a.run("assist.chatgpt.cancelSignIn", json!({})).unwrap();
}

/// The pane, while ChatGPT waits for a sign-in, says what to do once, with the button under it:
/// not "Sign in: Sign in with …".
#[test]
fn the_pane_says_how_to_sign_in_once() {
    let (folder, server) = (Folder::new(), serve());
    let mut a = window(&folder, &server);
    a.run("assist.backend", json!({"id": ID})).unwrap();
    a.run("assist.pane", json!({"value": true})).unwrap();
    let h = harness(a);
    use egui_kittest::kittest::Queryable;
    assert!(h.query_by_label("Sign in with a ChatGPT Plus or Pro account in Options › Account.").is_some());
    assert!(h.query_by_label_contains("Sign in: ").is_none());
    assert!(h.query_by_label("Continue with ChatGPT").is_some());
}

/// The two models the issue's list has, in its order: `gpt-6.1-sol` first.
fn the_list() -> (u16, String) {
    let list = json!({"models": [
        {"slug": "gpt-6.1-sol", "display_name": "GPT-6.1-Sol", "default_reasoning_level": "low",
         "supported_reasoning_levels": [{"effort": "low"}, {"effort": "medium"}, {"effort": "ultra"}]},
        {"slug": "gpt-6-luna", "display_name": "GPT-6-Luna", "description": "Quick", "default_reasoning_level": "medium",
         "supported_reasoning_levels": [{"effort": "low"}, {"effort": "medium"}, {"effort": "max"}]},
    ]});
    (200, list.to_string())
}

/// A window signed in at `server`, with the pane open on ChatGPT.
fn on_chatgpt(folder: &Folder, server: &Server) -> WordApp {
    let mut a = window(folder, server);
    sign_in(&mut a, server);
    a.ui.assistant = ID.into();
    a.run("assist.pane", json!({"value": true})).unwrap();
    a
}

fn read_list(a: &mut WordApp) {
    until(a, "the list was read", |a| {
        !matches!(a.assist.chatgpt.list, super::super::models::ModelList::Reading(_) | super::super::models::ModelList::Unread)
    });
}

fn ids(rows: &[crate::assist::models::Row]) -> Vec<&str> {
    rows.iter().map(|r| r.id.as_str()).collect()
}

#[test]
fn the_menus_list_the_accounts_models_with_gpt_6_luna_at_medium_as_the_default() {
    use crate::assist::models::menus;
    let (folder, server) = (Folder::new(), serve());
    *server.script.models.lock().unwrap() = Some(the_list());
    let mut a = on_chatgpt(&folder, &server);
    read_list(&mut a);
    let m = menus(&a).unwrap();
    assert_eq!(ids(&m.models), ["gpt-6.1-sol", "gpt-6-luna"]);
    assert_eq!((m.default_model.as_deref(), m.running_model.as_deref(), m.chosen_model.clone()), (Some("GPT-6-Luna"), Some("GPT-6-Luna"), None));
    assert_eq!(ids(&m.efforts), ["low", "medium", "max"]);
    assert_eq!(m.effort.as_deref(), Some("medium"));
    // The other model: its own levels, and the level that runs with nothing chosen tagged, which
    // isn't the one its list entry calls its default.
    a.execute("assist.model", json!({"id": "gpt-6.1-sol"})).unwrap();
    let m = menus(&a).unwrap();
    assert_eq!(ids(&m.efforts), ["low", "medium", "ultra"]);
    assert_eq!(m.efforts.iter().filter(|e| e.is_default).map(|e| e.id.as_str()).collect::<Vec<_>>(), ["medium"]);
    // A level of the other model is refused by this one, and a saved one that isn't offered falls
    // back here as it does in the turn.
    assert!(a.execute("assist.effort", json!({"level": "max"})).is_err());
    a.execute("assist.effort", json!({"level": "ultra"})).unwrap();
    a.execute("assist.model", json!({"id": null})).unwrap();
    let m = menus(&a).unwrap();
    assert_eq!((m.chosen_model.clone(), m.effort.as_deref()), (None, Some("medium")));
    assert_eq!(a.session.prefs.assistant.models[ID].effort.as_deref(), Some("ultra"), "it stays saved");
    assert!(a.execute("assist.model", json!({"id": "gone"})).unwrap_err().contains("gpt-6-luna"));
    // A saved model that left the list ticks the Default row, and what runs is named.
    a.session.prefs.assistant.models.get_mut(ID).unwrap().model = Some("gone".into());
    let m = menus(&a).unwrap();
    assert_eq!((m.chosen_model.clone(), m.running_model.as_deref()), (None, Some("GPT-6-Luna")));
    // The list was read once, for the pane.
    assert_eq!(server.script.asked.lock().unwrap().iter().filter(|p| *p == "/v1/models").count(), 1);
}

#[test]
fn a_list_with_no_levels_offers_low_medium_and_high() {
    let (folder, server) = (Folder::new(), serve());
    *server.script.models.lock().unwrap() = Some((200, json!({"data": [{"id": "gpt-x", "object": "model"}]}).to_string()));
    let mut a = on_chatgpt(&folder, &server);
    read_list(&mut a);
    let m = crate::assist::models::menus(&a).unwrap();
    assert_eq!(ids(&m.efforts), ["low", "medium", "high"]);
    assert_eq!(m.effort.as_deref(), Some("medium"));
    assert_eq!(m.efforts.iter().filter(|e| e.is_default).map(|e| e.id.as_str()).collect::<Vec<_>>(), ["medium"]);
    // The list says nothing against another level, and the turn would send it.
    a.execute("assist.effort", json!({"level": "ultra"})).unwrap();
    let m = crate::assist::models::menus(&a).unwrap();
    assert_eq!(ids(&m.efforts), ["low", "medium", "high", "ultra"]);
    assert_eq!(m.effort.as_deref(), Some("ultra"));
}

#[test]
fn no_frame_waits_for_the_list_and_the_menus_say_so_meanwhile() {
    use crate::assist::models::{ModelList, menus};
    let (folder, server) = (Folder::new(), serve());
    *server.script.models.lock().unwrap() = Some(the_list());
    server.script.hold_models.store(true, Ordering::Relaxed);
    let mut a = on_chatgpt(&folder, &server);
    let started = Instant::now();
    for _ in 0..20 {
        crate::assist::tick(&mut a);
    }
    assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());
    assert!(matches!(a.assist.chatgpt.list, ModelList::Reading(_)));
    let m = menus(&a).unwrap();
    assert!(m.models.is_empty() && m.unknown_because.is_some() && m.failed.is_none());
    assert_eq!(ids(&m.efforts), ["low", "medium", "high"], "the usual levels can be chosen meanwhile");
    server.script.hold_models.store(false, Ordering::Relaxed);
    read_list(&mut a);
    assert_eq!(ids(&menus(&a).unwrap().models), ["gpt-6.1-sol", "gpt-6-luna"]);
}

#[test]
fn a_list_that_cant_be_read_says_why_and_is_tried_again_when_the_pane_opens() {
    use crate::assist::models::menus;
    let (folder, server) = (Folder::new(), serve());
    *server.script.models.lock().unwrap() = Some((500, "{}".into()));
    let mut a = on_chatgpt(&folder, &server);
    read_list(&mut a);
    let m = menus(&a).unwrap();
    assert!(m.failed.as_deref().is_some_and(|why| why.contains("couldn't be listed")), "{:?}", m.failed);
    assert!(!m.known && m.models.is_empty());
    // A choice can still be made: the turn runs it, as a saved choice gets past a failed read.
    a.execute("assist.model", json!({"id": "gpt-saved"})).unwrap();
    assert_eq!(menus(&a).unwrap().running_model.as_deref(), Some("gpt-saved"));
    for _ in 0..10 {
        crate::assist::tick(&mut a);
    }
    assert_eq!(server.script.asked.lock().unwrap().iter().filter(|p| *p == "/v1/models").count(), 1, "not again every frame");
    *server.script.models.lock().unwrap() = Some(the_list());
    a.run("assist.pane", json!({"value": false})).unwrap();
    a.run("assist.pane", json!({"value": true})).unwrap();
    read_list(&mut a);
    assert_eq!(ids(&menus(&a).unwrap().models), ["gpt-6.1-sol", "gpt-6-luna"]);
}

#[test]
fn the_menus_need_someone_signed_in_and_a_new_sign_in_reads_the_list_again() {
    use crate::assist::models::menus;
    let (folder, server) = (Folder::new(), serve());
    let mut a = window(&folder, &server);
    a.ui.assistant = ID.into();
    a.run("assist.pane", json!({"value": true})).unwrap();
    crate::assist::tick(&mut a);
    assert_eq!(menus(&a), None);
    assert!(matches!(a.assist.chatgpt.list, crate::assist::models::ModelList::Unread), "nobody to read it for");
    *server.script.models.lock().unwrap() = Some(the_list());
    sign_in(&mut a, &server);
    read_list(&mut a);
    assert!(menus(&a).is_some());
    a.run("assist.chatgpt.signOut", json!({})).unwrap();
    until(&mut a, "signed out", |a| status(a) == ChatGptStatus::SignedOut);
    assert_eq!(menus(&a), None);
    sign_in(&mut a, &server);
    read_list(&mut a);
    assert_eq!(server.script.asked.lock().unwrap().iter().filter(|p| *p == "/v1/models").count(), 2);
}

#[test]
fn a_program_can_read_the_menus_but_not_while_nothing_is_offered() {
    let (folder, server) = (Folder::new(), serve());
    *server.script.models.lock().unwrap() = Some(the_list());
    let mut a = on_chatgpt(&folder, &server);
    read_list(&mut a);
    let answer = a.execute("assist.model", json!({})).unwrap();
    assert_eq!(answer["backend"], ID);
    assert_eq!(answer["offered"]["runningModel"], "GPT-6-Luna");
    assert_eq!(answer["offered"]["models"][0]["id"], "gpt-6.1-sol");
    assert_eq!(answer["offered"]["efforts"][1], json!({"id": "medium", "name": null, "description": null, "isDefault": true}));
    assert_eq!(answer["running"], json!(null));
}

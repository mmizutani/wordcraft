//! Explicit, single-tester local web development server.

use std::fs::File;
use std::io::Read as _;
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use wordcraft_assist::chatgpt::{self, SignIn, SignInOptions};
use wordcraft_http::{Config, Request, Response, same_secret};

const PREFIX: &str = "/api/assistant/local/";
const META: &str = r#"<meta name="wordcraft-local-assistant" content="enabled">"#;
const MAX_BODY: usize = 64 << 10;
const MAX_ASSET: u64 = 128 << 20;

#[derive(Default)]
struct State {
    sign_in: Option<SignIn>,
    rewriting: bool,
}

impl State {
    fn poll(&mut self) {
        if self.sign_in.as_mut().is_some_and(|attempt| attempt.poll().is_some()) {
            self.sign_in = None;
        }
    }
}

struct App {
    dist: PathBuf,
    settings: PathBuf,
    authority: String,
    origin: String,
    launch_key: String,
    state: Mutex<State>,
}

pub(crate) fn serve(dist: &Path) -> Result<(), String> {
    let dist = dist.canonicalize().map_err(|e| format!("Couldn't open the Trunk output: {e}"))?;
    if !dist.join("index.html").is_file() {
        return Err("Build the web editor with trunk build before starting the local Assistant.".into());
    }
    let settings = wordcraft_control_key::settings_dir().ok_or("Couldn't find the WordCraft settings folder.")?.join("local-web-assistant");
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string())?;
    let authority = listener.local_addr().map_err(|e| e.to_string())?.to_string();
    let origin = format!("http://{authority}");
    let launch_key = launch_key()?;
    crate::write_stdout(|out| {
        use std::io::Write as _;
        writeln!(out, "Local WordCraft Assistant: {origin}/#wordcraft-local-assistant={launch_key}")
    })
    .map_err(|e| e.to_string())?;
    let app = App { dist, settings, authority, origin, launch_key, state: Mutex::default() };
    let config =
        Config { limits: wordcraft_http::Limits { body: MAX_BODY, ..wordcraft_http::Limits::default() }, max_connections: 8, ..Config::default() };
    wordcraft_http::serve(listener, config, Arc::new(move |req| app.handle(req))).map_err(|e| e.to_string())
}

impl App {
    fn handle(&self, req: &Request) -> Response {
        let response = if !exact_header(req, "host", &self.authority) {
            error(403, "This server is only available on its own loopback address.")
        } else if let Some(route) = req.path().strip_prefix(PREFIX) {
            self.api(req, route)
        } else if req.method == "GET" {
            self.asset(req.path())
        } else {
            error(405, "Method not allowed.")
        };
        response
            .with_header("Cache-Control", "no-store")
            .with_header("Content-Security-Policy", "frame-ancestors 'none'")
            .with_header("X-Frame-Options", "DENY")
            .with_header("Referrer-Policy", "no-referrer")
            .with_header("X-Content-Type-Options", "nosniff")
    }

    fn api(&self, req: &Request, route: &str) -> Response {
        if !single_header(req, "authorization").and_then(|value| value.strip_prefix("Bearer ")).is_some_and(|key| same_secret(key, &self.launch_key))
        {
            return error(403, "Open the full URL printed by the local WordCraft Assistant.");
        }
        if req.method == "GET" && route == "status" {
            let Ok(mut state) = self.state.lock() else { return unavailable() };
            state.poll();
            return reply(json!({"connected": self.connected(), "connecting": state.sign_in.is_some()}));
        }
        if req.method != "POST" || !matches!(route, "connect" | "disconnect" | "rewrite") {
            return error(404, "Assistant route not found.");
        }
        if !exact_header(req, "origin", &self.origin) {
            return error(403, "Open this WordCraft editor directly to use the Assistant.");
        }
        let Ok(mut state) = self.state.lock() else { return unavailable() };
        state.poll();
        // A provider retry may reload credentials. Keep the same grant until it finishes.
        if state.rewriting && matches!(route, "connect" | "disconnect") {
            return error(409, "Wait for the current Rewrite to finish before changing the ChatGPT connection.");
        }
        match route {
            "connect" => {
                if self.connected() || state.sign_in.is_some() {
                    return error(409, "A ChatGPT connection already exists.");
                }
                match SignIn::start_for_local_web(&self.settings, SignInOptions { port: 0, consent: true, ..SignInOptions::default() }) {
                    Ok(attempt) => {
                        let response = reply(json!({"authorization_url": attempt.url()}));
                        state.sign_in = Some(attempt);
                        response
                    }
                    Err(_) => error(502, "Couldn't start ChatGPT sign-in."),
                }
            }
            "disconnect" => {
                if let Some(attempt) = state.sign_in.as_mut() {
                    attempt.cancel();
                    // The callback might already be exchanging its code. Wait before deleting
                    // the grant so a late exchange cannot restore a disconnected session.
                    if attempt.wait(Duration::from_secs(100)).is_none() {
                        return error(503, "ChatGPT sign-in is finishing. Try disconnecting again.");
                    }
                    state.sign_in = None;
                }
                match chatgpt::sign_out(&self.settings) {
                    Ok(result) if result.revoked => reply(json!({"connected": false})),
                    Ok(_) => error(502, "Disconnected locally. Check ChatGPT Settings to revoke remote access."),
                    Err(_) => error(502, "Couldn't disconnect ChatGPT."),
                }
            }
            "rewrite" => {
                if !self.connected() {
                    return error(401, "Connect an eligible ChatGPT plan first.");
                }
                if state.rewriting {
                    return error(409, "A Rewrite is already running.");
                }
                let Some(body) = serde_json::from_slice::<Value>(&req.body).ok() else {
                    return error(400, "Provide selected text to rewrite.");
                };
                let Some(text) = body.get("text").and_then(Value::as_str).filter(|s| chatgpt::valid_local_rewrite(s)) else {
                    return error(400, "Select nonempty text within the Rewrite size limit.");
                };
                state.rewriting = true;
                drop(state);
                let result = chatgpt::local_rewrite(&self.settings, text);
                let Ok(mut state) = self.state.lock() else { return unavailable() };
                state.rewriting = false;
                if !self.connected() {
                    return error(401, "The ChatGPT connection ended. Connect again to rewrite.");
                }
                match result {
                    Ok(replacement) => reply(json!({"replacement": replacement})),
                    Err(chatgpt::LocalRewriteError::Usage) => error(429, "ChatGPT plan usage is unavailable. Check ChatGPT Settings → Usage."),
                    Err(_) => error(502, "ChatGPT didn't finish this Rewrite. You can try again."),
                }
            }
            _ => error(404, "Assistant route not found."),
        }
    }

    fn connected(&self) -> bool {
        chatgpt::account(&self.settings).is_some_and(|account| account.plan_usage)
    }

    fn asset(&self, path: &str) -> Response {
        let name = if path == "/" { "index.html" } else { path.strip_prefix('/').unwrap_or_default() };
        if name.is_empty()
            || !name.split('/').all(|component| {
                !component.is_empty()
                    && !component.starts_with('.')
                    && component.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
            })
        {
            return error(404, "Editor asset not found.");
        }
        let Ok(file) = self.dist.join(name).canonicalize() else { return error(404, "Editor asset not found.") };
        if !file.starts_with(&self.dist) || !file.is_file() {
            return error(404, "Editor asset not found.");
        }
        let content_type = match file.extension().and_then(|ext| ext.to_str()) {
            Some("html") if name == "index.html" => "text/html; charset=utf-8",
            Some("wasm") => "application/wasm",
            Some("js" | "mjs") => "text/javascript; charset=utf-8",
            Some("css") => "text/css; charset=utf-8",
            Some("ico") => "image/x-icon",
            _ => return error(404, "Editor asset not found."),
        };
        let mut bytes = Vec::new();
        let read = File::open(file).and_then(|f| f.take(MAX_ASSET + 1).read_to_end(&mut bytes));
        if read.is_err() || bytes.len() as u64 > MAX_ASSET {
            return error(500, "Couldn't read the editor asset.");
        }
        if name == "index.html" {
            let Ok(html) = std::str::from_utf8(&bytes) else { return error(500, "Couldn't read the editor page.") };
            let Some((head, rest)) = html.split_once("</head>") else { return error(500, "Couldn't read the editor page.") };
            bytes = format!("{head}{META}</head>{rest}").into_bytes();
        }
        Response::new(200).with_header("Content-Type", content_type).with_body(bytes)
    }
}

fn launch_key() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| "Couldn't create the local Assistant launch key.")?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn exact_header(req: &Request, name: &str, expected: &str) -> bool {
    single_header(req, name) == Some(expected)
}

fn single_header<'a>(req: &'a Request, name: &str) -> Option<&'a str> {
    let mut values = req.headers.iter().filter(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.trim());
    let value = values.next()?;
    values.next().is_none().then_some(value)
}

fn reply(body: Value) -> Response {
    Response::json(200, body.to_string())
}

fn error(status: u16, message: &str) -> Response {
    Response::json(status, json!({"error": message}).to_string())
}

fn unavailable() -> Response {
    error(500, "The local Assistant is unavailable.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_routes_require_the_launch_key_even_with_the_local_host_and_origin() {
        let app = test_app("capability");
        // Keep Connect isolated from live OAuth if this test detects an authorization regression.
        app.state.lock().unwrap().rewriting = true;
        for (method, route) in [("GET", "status"), ("POST", "connect"), ("POST", "disconnect"), ("POST", "rewrite")] {
            let mut req = assistant_request(&app, method, route);
            assert_eq!(app.handle(&req).status, 403, "{route} accepted a missing key");
            req.headers.push(("Authorization".into(), format!("Bearer {}", launch_key().unwrap())));
            assert_eq!(app.handle(&req).status, 403, "{route} accepted an invalid key");
            req.headers[2].1 = format!("Bearer {}", app.launch_key);
            req.headers.push(("authorization".into(), format!("Bearer {}", app.launch_key)));
            assert_eq!(app.handle(&req).status, 403, "{route} accepted duplicate credentials");
        }
    }

    #[test]
    fn another_tab_cannot_change_the_grant_during_a_rewrite() {
        let app = test_app("rewrite-race");
        app.state.lock().unwrap().rewriting = true;
        for route in ["disconnect", "connect"] {
            let mut req = assistant_request(&app, "POST", route);
            req.headers.push(("Authorization".into(), format!("Bearer {}", app.launch_key)));
            let response = app.handle(&req);
            if response.status != 409 {
                let _ = std::fs::remove_dir_all(&app.settings);
            }
            assert_eq!(response.status, 409, "{route} accepted during Rewrite");
        }
        assert!(!app.settings.exists());
    }

    fn test_app(label: &str) -> App {
        let authority = "127.0.0.1:8771".to_string();
        App {
            dist: PathBuf::new(),
            settings: std::env::temp_dir().join(format!("wordcraft-local-assistant-{label}-{}", std::process::id())),
            origin: format!("http://{authority}"),
            authority,
            launch_key: launch_key().unwrap(),
            state: Mutex::default(),
        }
    }

    fn assistant_request(app: &App, method: &str, route: &str) -> Request {
        Request {
            method: method.into(),
            target: format!("{PREFIX}{route}"),
            headers: vec![("Host".into(), app.authority.clone()), ("Origin".into(), app.origin.clone())],
            ..Request::default()
        }
    }

    #[test]
    fn only_the_local_origin_can_use_assistant_routes() {
        let dist = std::env::temp_dir().join(format!("wordcraft-local-assistant-assets-{}", std::process::id()));
        std::fs::create_dir_all(&dist).unwrap();
        std::fs::write(dist.join("index.html"), "<html><head><title>Test</title></head></html>").unwrap();
        std::fs::write(dist.join("editor.js"), "export {};").unwrap();
        let mut app = test_app("origin");
        app.dist = dist.canonicalize().unwrap();
        let mut req =
            Request { method: "GET".into(), target: "/".into(), headers: vec![("Host".into(), app.authority.clone())], ..Request::default() };

        let index = app.handle(&req);
        assert_eq!(index.status, 200);
        let page = std::str::from_utf8(&index.body).unwrap();
        assert!(page.contains(META));
        assert!(!page.contains(&app.launch_key));
        assert_eq!(index.header("cache-control"), Some("no-store"));
        assert_eq!(index.header("x-frame-options"), Some("DENY"));
        req.target = "/editor.js".into();
        assert_eq!(app.handle(&req).body, b"export {};");
        req.target = "/../index.html".into();
        assert_eq!(app.handle(&req).status, 404);

        req.target = format!("{PREFIX}status");
        req.headers.push(("Authorization".into(), format!("Bearer {}", app.launch_key)));
        assert_eq!(serde_json::from_slice::<Value>(&app.handle(&req).body).unwrap(), json!({"connected":false,"connecting":false}));

        req.method = "POST".into();
        req.target = format!("{PREFIX}rewrite");
        assert_eq!(app.handle(&req).status, 403);
        req.headers.push(("Origin".into(), "https://example.test".into()));
        assert_eq!(app.handle(&req).status, 403);
        req.headers[2].1 = app.origin.clone();
        assert_eq!(app.handle(&req).status, 401);
        req.headers.push(("Origin".into(), app.origin.clone()));
        assert_eq!(app.handle(&req).status, 403);

        req.headers.truncate(3);
        req.headers[0].1 = "rebound.example".into();
        assert_eq!(app.handle(&req).status, 403);
        assert!(!app.settings.exists());
        std::fs::remove_dir_all(&dist).unwrap();
    }
}

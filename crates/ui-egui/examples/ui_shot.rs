//! Headless UI screenshots: renders the whole WordCraft window offscreen (wgpu), so it works
//! with a locked screen or a hidden window.
//!
//! `cargo run -p wordcraft-ui-egui --example ui_shot -- script.jsonl`
//!
//! The script is JSON lines: control-channel requests (`{"method": …, "params": …}`, see
//! `docs/control-protocol.md`), `{"shot": "/abs/out.png"}` to save the window as PNG, or
//! `{"steps": n}` to run extra frames, or `{"wait": ms}` to keep running frames for a while (for
//! background work such as a Zotero session). The window is 1440×900 pt at 2× and opens the sample
//! document unless the first line is `{"empty": true}`; `{"empty": true, "startScreen": true}`
//! starts the way the desktop app does with nothing to open (File › Home, #766). A line
//! `{"demoPacks": true}` lists a few sample Language packs (installs only record themselves), and
//! (with `{"network": true}` on a line, downloading is already on) `{"demoPacks": "store"}` uses a real font store, as the desktop app does, in a new temporary
//! folder rather than the person's, or in `"storeDir"` given on the same line (with
//! `CRAFT_LANGUAGE_PACK_INDEX` for a local pack index). `{"assistScript": [[step, …], …]}` has the
//! next assistant conversation use the scripted assistant, one list of steps per turn: `{"say":
//! text}`, `{"think": text}`, `{"tool": name, "args": {…}}`, `{"plan": [text, …]}` (an entry
//! may be `{"content": text, "status": "completed|in_progress|pending"}`) or `{"ask": title}` (a
//! permission request: the pane asks it in a turn the person asked for, such as one started with
//! `{"run": "assist.ask", "params": {"prompt": …}}`, and answers it per `assist.allowUnattended`
//! in a program's: one from `engine.execute`, or typed with `ui.text` and sent with `ui.key`,
//! which the window doesn't take as the person's). `{"run": id, "params": {…}}` runs command `id`
//! as the person's own click does (`WordApp::run`), for what a script may not do, such as
//! signing in to ChatGPT (`assist.chatgpt.signIn`, #19; no browser opens here).
//! `{"settingsDir": "/abs/dir"}` gives the window a settings folder (the agents' settings in
//! File › Options › Assistant read and save there, and a ChatGPT sign-in is kept there), and
//! `{"web": true}` has it offer what the web app does: no assistant. `{"room": "127.0.0.1:7982"}`
//! gives the window a lobby whose room port says it listens there (nothing listens), so a shared
//! document can be joined: `{"join": {"name", "role"?, "files"?, "copies"?}}` invites a participant
//! and joins with their key (`copies` times, once by default), and `{"inviteOnPage": {…}}` invites
//! as File › Share's Invite does, showing the key on the page (#33).
//! Once the document is shared (`{"method": "room.share"}`), `{"agent": name, "role"?: role,
//! "run": [[command, params], …]}` invites a participant called `name` (an editor unless `role`
//! says otherwise), joins with the key in a copy of their own, as an agent does over the control
//! channel, and runs the commands there (`select.text`, `caret.set`, `text.insert`…), so the window
//! shows their caret, selection and name (#32); the next line naming the same agent runs more
//! commands in the same copy.

use wordcraft_engine::Session;
use wordcraft_ui_egui::{ControlRequest, Services, WordApp};

static READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn main() -> Result<(), String> {
    let path = std::env::args().nth(1).expect("usage: ui_shot script.jsonl");
    let script = std::fs::read_to_string(&path).expect("read script");
    let (tx, rx) = std::sync::mpsc::channel::<ControlRequest>();
    let lines: Vec<serde_json::Value> =
        script.lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).expect("json line")).collect();
    let doc = if lines.first().is_some_and(|l| l.get("empty").is_some()) {
        wordcraft_doc::Document::new()
    } else {
        wordcraft_engine::sample::sample_document()
    };
    match lines.iter().find_map(|l| l.get("demoPacks")) {
        Some(serde_json::Value::String(s)) if s == "store" => {
            // `"storeDir"` on the same line names the folder (to show packs put there beforehand).
            let dir = lines
                .iter()
                .find_map(|l| l.get("storeDir").and_then(|v| v.as_str()))
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::env::temp_dir().join(format!("ui_shot-font-store-{}", std::process::id())));
            wordcraft_engine::packs::use_font_store_in(dir, true);
        }
        Some(_) => {
            // `"network": true` on a line has downloading already on (to show what follows a click on Add a language…).
            let backend = wordcraft_engine::packs::FixedBackend::sample();
            if lines.iter().any(|l| l.get("network") == Some(&serde_json::Value::Bool(true))) {
                backend.network.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            wordcraft_engine::packs::set_backend(Some(std::sync::Arc::new(backend)));
        }
        None => {}
    }
    // `{"settingsDir": "/abs/dir"}`: the settings folder, where File › Options › Assistant keeps
    // the agents' settings (#24); none otherwise, as in tests.
    let settings_dir = lines.iter().find_map(|l| l.get("settingsDir").and_then(|v| v.as_str())).map(std::path::PathBuf::from);
    let mut app = WordApp::new(Session::new(doc), Services { settings_dir, ..Services::default() }).with_control(rx);
    // `{"room": addr}`: a lobby for the shared document, where `{"join": …}` lines join it; the
    // lobby is there without it too, for `{"agent": …}` lines.
    let lobby = wordcraft_engine::room::Lobby::default();
    if let Some(addr) = lines.iter().find_map(|l| l.get("room").and_then(|v| v.as_str())) {
        lobby.set_addr(Some(addr.parse().map_err(|e| format!("room {addr}: {e}"))?));
    }
    app = app.with_lobby(lobby.clone());
    // The participants' copies, kept in the room till the script ends.
    let mut copies = Vec::new();
    let mut agents: Vec<(String, wordcraft_engine::room::Replica)> = Vec::new();
    app.integrated_titlebar = true;
    if lines.first().is_some_and(|l| l.get("startScreen").is_some()) {
        app.start_screen();
    }
    let w = lines.iter().find_map(|l| l.get("width").and_then(|v| v.as_f64())).unwrap_or(1440.0) as f32;
    let h = lines.iter().find_map(|l| l.get("height").and_then(|v| v.as_f64())).unwrap_or(900.0) as f32;
    let mut harness =
        egui_kittest::Harness::builder().with_size(egui::vec2(w, h)).with_pixels_per_point(2.0).with_max_steps(1_000_000).wgpu().build_ui_state(
            |ui, app: &mut WordApp| {
                if !READY.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
    harness.input_mut().max_texture_side = Some(8192);
    READY.store(true, std::sync::atomic::Ordering::Relaxed);
    step_n(&mut harness, 6);
    for l in lines {
        if let Some(p) = l.get("shot").and_then(|v| v.as_str()) {
            step_n(&mut harness, 4);
            match harness.render() {
                Ok(img) => {
                    img.save(p).expect("save png");
                    println!("{p}");
                }
                Err(e) => eprintln!("render failed: {e}"),
            }
        } else if l.get("web") == Some(&serde_json::Value::Bool(true)) {
            // The pane as the web app has it: no assistant (#26).
            harness.state_mut().assist_as_web();
            step_n(&mut harness, 2);
        } else if let Some(turns) = l.get("assistScript").and_then(|v| v.as_array()) {
            harness.state_mut().assist_with(Box::new(scripted_assistant(turns)?));
            step_n(&mut harness, 2);
        } else if let Some(id) = l.get("run").and_then(|v| v.as_str()) {
            // A command as the person runs it, which a script can't: a prompt whose agent's
            // permission requests the pane asks (#23), Continue with ChatGPT (#19).
            let r = harness.state_mut().run(id, l.get("params").cloned().unwrap_or_else(|| serde_json::json!({})));
            println!("{id}: {}", r.map_or_else(|e| e, |v| v.to_string()));
            step_n(&mut harness, 2);
        } else if let Some(name) = l.get("agent").and_then(|v| v.as_str()) {
            agent_line(&mut harness, &tx, &lobby, &mut agents, name, &l);
        } else if let Some(j) = l.get("join") {
            let room = lobby.room().ok_or("join: share the document first (room.share)")?;
            let name = j.get("name").and_then(|v| v.as_str()).ok_or("join: a name")?;
            let role =
                j.get("role").and_then(|v| v.as_str()).map_or(Some(wordcraft_engine::room::Role::Suggester), wordcraft_engine::room::Role::named);
            let files = j.get("files").and_then(|v| v.as_str()).and_then(wordcraft_engine::agent_files::AgentFiles::named);
            let key = room.invite_with(name, role.ok_or("join: an unknown role")?, files).map_err(|e| e.to_string())?;
            for _ in 0..j.get("copies").and_then(|v| v.as_u64()).unwrap_or(1) {
                copies.push(lobby.join_with(&key).map_err(|e| e.to_string())?);
            }
            step_n(&mut harness, 2);
        } else if let Some(p) = l.get("inviteOnPage") {
            println!("inviteOnPage: {:?}", harness.state_mut().invite_on_share_page(p.clone()));
            step_n(&mut harness, 2);
        } else if let Some(n) = l.get("steps").and_then(|v| v.as_u64()) {
            step_n(&mut harness, n as usize);
        } else if let Some(ms) = l.get("wait").and_then(|v| v.as_u64()) {
            let end = std::time::Instant::now() + std::time::Duration::from_millis(ms);
            while std::time::Instant::now() < end {
                step(&mut harness);
                std::thread::sleep(std::time::Duration::from_millis(16));
            }
        } else if let Some(m) = l.get("method").and_then(|v| v.as_str()) {
            let (req, reply) = ControlRequest::new(m, l.get("params").cloned().unwrap_or_default());
            tx.send(req).expect("send");
            for _ in 0..60 {
                step(&mut harness);
                if let Ok(r) = reply.try_recv() {
                    let s = r.to_string();
                    // `UI_SHOT_FULL=1` prints whole replies (e.g. `document.inspect` for scripted checks).
                    let cut = if std::env::var_os("UI_SHOT_FULL").is_some() { s.len() } else { s.len().min(300) };
                    println!("{m}: {}", s.get(..cut).unwrap_or(&s));
                    break;
                }
            }
            step_n(&mut harness, 2);
        }
    }
    Ok(())
}

/// The scripted assistant `{"assistScript": …}` describes.
/// An `{"agent": name, "role"?, "run": [...]}` line: invites `name` the first time (through the
/// control channel, as an owner's script does) and joins with the key in a copy of their own, then
/// runs the line's commands in that copy.
fn agent_line(
    harness: &mut egui_kittest::Harness<'_, WordApp>,
    tx: &std::sync::mpsc::Sender<ControlRequest>,
    lobby: &wordcraft_engine::room::Lobby,
    agents: &mut Vec<(String, wordcraft_engine::room::Replica)>,
    name: &str,
    l: &serde_json::Value,
) {
    if !agents.iter().any(|(n, _)| n == name) {
        let role = l.get("role").and_then(|v| v.as_str()).unwrap_or("editor");
        let (req, reply) = ControlRequest::new("room.invite", serde_json::json!({"name": name, "role": role}));
        tx.send(req).expect("send");
        let mut key = None;
        for _ in 0..60 {
            step(harness);
            if let Ok(r) = reply.try_recv() {
                key = r.pointer("/result/key").and_then(|k| k.as_str()).map(str::to_string);
                if key.is_none() {
                    eprintln!("room.invite {name}: {r}");
                }
                break;
            }
        }
        let Some(key) = key else { return };
        match lobby.join_with(&key) {
            Ok(copy) => agents.push((name.to_string(), copy)),
            Err(e) => {
                eprintln!("{name} joins: {e}");
                return;
            }
        }
    }
    if let Some((_, copy)) = agents.iter_mut().find(|(n, _)| n == name) {
        for c in l.get("run").and_then(|v| v.as_array()).into_iter().flatten() {
            let (id, params) = (c.get(0).and_then(|v| v.as_str()).unwrap_or_default(), c.get(1).cloned().unwrap_or_default());
            let r = wordcraft_engine::room::hosted::call(copy, "engine.execute", &serde_json::json!({"command": id, "params": params}));
            println!("{name} {id}: {}", r.map_or_else(|e| e, |v| v.to_string()));
        }
    }
    step_n(harness, 4);
}

fn scripted_assistant(turns: &[serde_json::Value]) -> Result<wordcraft_assist::Fake, String> {
    use wordcraft_assist::{PlanEntry, PlanStatus, Priority, Step};
    let text = |v: &serde_json::Value| v.as_str().unwrap_or_default().to_string();
    let step = |s: &serde_json::Value| {
        Ok(if let Some(t) = s.get("say") {
            Step::Say(text(t))
        } else if let Some(t) = s.get("think") {
            Step::Think(text(t))
        } else if let Some(name) = s.get("tool") {
            Step::tool(&text(name), s.get("args").cloned().unwrap_or_default())
        } else if let Some(entries) = s.get("plan").and_then(|v| v.as_array()) {
            // An entry is its text (the first done, the rest to come) or `{"content", "status"}`.
            let status = |i: usize, e: &serde_json::Value| match e.get("status").and_then(|s| s.as_str()) {
                Some("completed") => PlanStatus::Completed,
                Some("in_progress") => PlanStatus::InProgress,
                Some(_) => PlanStatus::Pending,
                None if i == 0 => PlanStatus::Completed,
                None => PlanStatus::Pending,
            };
            let content = |e: &serde_json::Value| e.get("content").map_or_else(|| text(e), text);
            Step::Plan(
                entries
                    .iter()
                    .enumerate()
                    .map(|(i, e)| PlanEntry { content: content(e), priority: Priority::Medium, status: status(i, e) })
                    .collect(),
            )
        } else if let Some(t) = s.get("ask") {
            Step::Ask(text(t))
        } else {
            return Err(format!("an assistScript step is say, think, tool, plan or ask: {s}"));
        })
    };
    turns.iter().try_fold(wordcraft_assist::Fake::new(), |fake, turn| {
        let steps = turn.as_array().ok_or("an assistScript turn must be an array of steps")?;
        Ok(fake.then(steps.iter().map(step).collect::<Result<Vec<_>, String>>()?))
    })
}

/// Applies the harness's pending raw input and advances one UI frame.
fn step(harness: &mut egui_kittest::Harness<'_, WordApp>) {
    let mut raw = std::mem::take(harness.input_mut());
    let ctx = harness.ctx.clone();
    harness.state_mut().raw_input_hook(&ctx, &mut raw);
    *harness.input_mut() = raw;
    harness.step();
}

fn step_n(harness: &mut egui_kittest::Harness<'_, WordApp>, n: usize) {
    for _ in 0..n {
        step(harness);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn unsupported_assistant_steps_do_not_panic() {
        let turns = vec![serde_json::json!([{}])];
        assert!(super::scripted_assistant(&turns).is_err());
        assert!(super::scripted_assistant(&[serde_json::json!(null)]).is_err());
        assert!(super::scripted_assistant(&[serde_json::json!([{"say": "Hello"}])]).is_ok());
    }
}

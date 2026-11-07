//! `wordcraft-relay`: run a relay, so agents on other machines reach documents shared from
//! desktops that dial out to it. See `docs/relay.md`.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::process::ExitCode;
use std::sync::Arc;

use wordcraft_relay::server::{Config, Relay, serve};

const USAGE: &str = "\
wordcraft-relay --public-url URL [--listen ADDR] [--tokens-file FILE] [--trust-forwarded] [--max-connections N]

  --public-url URL     the relay's address as agents and desktops reach it: https://… (TLS ends
                       at the proxy in front of the relay), or http://127.0.0.1:PORT to try it
  --listen ADDR        where to listen (default 127.0.0.1:8790); put a TLS proxy in front
  --tokens-file FILE   one relay token per line, for desktops to open rooms with; also
                       WORDCRAFT_RELAY_TOKENS (comma-separated). Tokens are 32 characters or more
  --trust-forwarded    take agents' addresses from X-Forwarded-For (set by the proxy in front)
  --max-connections N  connections served at once (default 512)
";

fn value(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn run(args: &[String]) -> Result<(), String> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return Ok(());
    }
    let known = ["--public-url", "--listen", "--tokens-file", "--trust-forwarded", "--max-connections"];
    let mut i = 0;
    while let Some(a) = args.get(i) {
        if !known.contains(&a.as_str()) {
            return Err(format!("unknown argument `{a}`\n\n{USAGE}"));
        }
        i += if a == "--trust-forwarded" { 1 } else { 2 };
    }
    let public_url = wordcraft_relay::relay_url(&value(args, "--public-url").ok_or(format!("--public-url is needed\n\n{USAGE}"))?)?;
    let listen = value(args, "--listen").unwrap_or_else(|| "127.0.0.1:8790".into());
    let mut tokens: Vec<String> =
        std::env::var("WORDCRAFT_RELAY_TOKENS").unwrap_or_default().split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
    if let Some(file) = value(args, "--tokens-file") {
        let text = std::fs::read_to_string(&file).map_err(|e| format!("{file}: {e}"))?;
        tokens.extend(text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(str::to_string));
    }
    if tokens.is_empty() {
        return Err("no relay tokens: give --tokens-file or WORDCRAFT_RELAY_TOKENS, or no desktop can open a room".into());
    }
    if let Some(short) = tokens.iter().find(|t| t.len() < 32) {
        return Err(format!("a relay token is {} characters: make each 32 or more (for example `openssl rand -hex 32`)", short.len()));
    }
    let max_connections = match value(args, "--max-connections") {
        Some(n) => n.parse::<usize>().ok().filter(|n| *n > 0).ok_or("--max-connections needs a number from 1")?,
        None => 512,
    };
    let config =
        Config { public_url: public_url.clone(), tokens, trust_forwarded: args.iter().any(|a| a == "--trust-forwarded"), ..Config::default() };
    let listener = std::net::TcpListener::bind(&listen).map_err(|e| format!("cannot listen on {listen}: {e}"))?;
    eprintln!("wordcraft-relay: listening on {listen}; rooms at {public_url}/r/<room>/mcp");
    serve(listener, Arc::new(Relay::new(config)), max_connections).map_err(|e| e.to_string())
}

/// The relay's log on stderr: rooms opened and closed, by the start of their id.
struct Stderr;

impl log::Log for Stderr {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::Level::Info
    }
    fn log(&self, r: &log::Record) {
        if self.enabled(r.metadata()) {
            eprintln!("wordcraft-relay: {}", r.args());
        }
    }
    fn flush(&self) {}
}

static LOG: Stderr = Stderr;

fn main() -> ExitCode {
    if log::set_logger(&LOG).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("wordcraft-relay: {e}");
            ExitCode::from(2)
        }
    }
}

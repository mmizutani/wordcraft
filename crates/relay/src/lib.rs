//! WordCraft's relay (#76): an agent on another machine reaches a document shared from a person's
//! desktop, and nobody runs a tunnel. The desktop dials out to the relay ([`host`]): it opens a
//! room there and then asks, again and again over HTTPS, for the requests agents sent to the
//! room's URL, answering each one. Agents use the room's URL, `https://<relay>/r/<room>/mcp`, as
//! an MCP server, with their invite's key in `Authorization: Bearer` ([`server`]).
//!
//! The relay checks no invite key and keeps none: it forwards the request, and the desktop
//! checks the key as it would for a connection on its own computer, so a relay that is broken
//! into can't let anyone in. It does see the requests and answers (TLS ends at the relay, as at
//! any tunnel service): whoever runs it is trusted with what the agents do. One binary,
//! `wordcraft-relay`, for anyone to run (`docs/relay.md`).
//!
//! Desktop ↔ relay, all with the room's secret in `Authorization: Bearer` except the first:
//!
//! - `POST /host/rooms` (a relay token): `{}` opens a room, `{"room", "secret"}` opens that one
//!   again (after the relay restarted) → `{"room", "secret", "url"}`.
//! - `POST /host/r/<room>/poll` → `{"requests": [{"id", "method", "target", "headers", "body"}]}`,
//!   waiting up to 25 s for one.
//! - `POST /host/r/<room>/reply` `{"id", "status", "headers", "body"}` → 204.
//! - `DELETE /host/r/<room>` closes the room.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod server;
pub mod wire;

#[cfg(not(target_arch = "wasm32"))]
pub mod host;

/// `len` random bytes as lowercase hex.
pub fn random_hex(len: usize) -> Result<String, String> {
    let mut bytes = vec![0u8; len];
    getrandom::fill(&mut bytes).map_err(|e| format!("no random bytes: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// A room's id, from its secret: the first 128 bits of a SHA-256 of it, as hex. The relay keeps
/// nothing on disk, so this is what ties a room's URL to the desktop that holds its secret: after
/// a restart, or once the relay has forgotten the room, only that desktop can open it again.
pub fn room_of(secret: &str) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::new().chain_update(b"wordcraft-relay room\0").chain_update(secret.as_bytes()).finalize();
    hash.iter().take(16).map(|b| format!("{b:02x}")).collect()
}

/// Whether `s` is `len` lowercase hex digits.
pub fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Whether `url` may be a relay's address: `https://`, or `http://` to this computer only (for
/// trying a relay out, and for tests). Returns it without a trailing `/`.
pub fn relay_url(url: &str) -> Result<String, String> {
    let url = url.trim().trim_end_matches('/');
    let local = ["http://127.0.0.1:", "http://localhost:", "http://[::1]:"].iter().any(|p| url.starts_with(p))
        || ["http://127.0.0.1", "http://localhost"].contains(&url);
    let https = url.strip_prefix("https://").is_some_and(|rest| !rest.is_empty() && !rest.contains(['?', '#', ' ', '@']));
    // No user part: `http://127.0.0.1:80@elsewhere` goes elsewhere.
    if (https || local) && !url.contains('@') {
        Ok(url.to_string())
    } else {
        Err(format!("`{url}` isn't a relay's address: it starts with https:// (or http://127.0.0.1 to try one on this computer)"))
    }
}

#[cfg(test)]
mod tests;

//! HTTP for the sign-in and the API: `ureq` agents with timeouts and bounded bodies, URL and
//! form encoding, and the redaction every error text goes through before the person sees it.
//!
//! ureq-proto's `trace` records are the raw bytes of each request and answer (a bearer token, the
//! token endpoint's answers): a logger must keep the `ureq_proto` and `ureq` targets at `debug`
//! or quieter, whatever it is asked for. The desktop app's logger does.

use std::io::Read;
use std::time::Duration;

/// Longest a connection takes to open.
pub(crate) const CONNECT: Duration = Duration::from_secs(15);

/// Longest a whole call that doesn't stream takes (the models list), and a streamed answer's
/// start.
pub(crate) const CALL: Duration = Duration::from_secs(180);

/// Longest a whole call to the sign-in server takes (the token endpoint, its configuration, the
/// revocation): a refresh holds the credentials' lock across one.
pub(crate) const AUTH_CALL: Duration = Duration::from_secs(30);

/// Longest one read of a streamed answer waits for its next bytes.
pub(crate) const STREAM_READ: Duration = Duration::from_secs(120);

/// Most bytes of an answer that doesn't stream.
pub(crate) const MAX_BODY: u64 = 8 << 20;

/// Most bytes of an error answer read (the rest is left unread).
pub(crate) const MAX_ERROR_BODY: u64 = 64 << 10;

/// Most characters of a server's message shown to the person.
const MAX_MESSAGE: usize = 300;

/// An answer that was read whole.
#[derive(Clone, Debug, Default)]
pub(crate) struct Reply {
    pub(crate) status: u16,
    pub(crate) body: Vec<u8>,
    /// `Retry-After`, in seconds, when the server said one.
    pub(crate) retry_after: Option<Duration>,
    /// The server's request id (`openai-request-id` or `x-request-id`), for the log.
    pub(crate) request_id: Option<String>,
}

impl Reply {
    /// Whether the status is 2xx.
    pub(crate) fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// The body as JSON, or null.
    pub(crate) fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or(serde_json::Value::Null)
    }
}

/// The agent API requests go through: no redirects (tokens stay on the host they were sent to),
/// statuses as answers rather than errors, timeouts, and no proxy for servers on this machine.
/// A streamed answer has [`CALL`] to start and [`STREAM_READ`] between reads; any other call
/// has [`CALL`] in all.
pub(crate) fn agent(local: bool, streaming: bool) -> ureq::Agent {
    agent_with(local, if streaming { None } else { Some(CALL) })
}

/// The agent the sign-in server's calls go through: [`agent`]'s, with [`AUTH_CALL`] for each
/// whole call.
pub(crate) fn auth_agent(local: bool) -> ureq::Agent {
    agent_with(local, Some(AUTH_CALL))
}

/// An agent whose calls take at most `whole` each, or (None) that streams.
fn agent_with(local: bool, whole: Option<Duration>) -> ureq::Agent {
    let mut config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_connect(Some(CONNECT))
        .user_agent(format!("WordCraft/{}", env!("CARGO_PKG_VERSION")));
    config = match whole {
        Some(whole) => config.timeout_global(Some(whole)),
        None => config.timeout_recv_response(Some(CALL)).timeout_per_read(Some(STREAM_READ)),
    };
    if local {
        config = config.proxy(None);
    }
    config.build().into()
}

/// Read an answer whole: at most `max` bytes of its body, else an error.
pub(crate) fn read(response: ureq::http::Response<ureq::Body>, max: u64) -> Result<Reply, String> {
    let status = response.status().as_u16();
    let header = |name: &str| response.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
    let retry_after = header("retry-after").and_then(|v| v.trim().parse::<u64>().ok()).map(Duration::from_secs);
    let request_id =
        header("openai-request-id").or_else(|| header("x-request-id")).map(|id| id.chars().filter(char::is_ascii_graphic).take(128).collect());
    // An error answer is only for its message: its start is enough.
    let ok = (200..300).contains(&status);
    let limit = if ok { max } else { max.min(MAX_ERROR_BODY) };
    let mut body = Vec::new();
    let mut reader = body_reader(response).take(limit.saturating_add(1));
    reader.read_to_end(&mut body).map_err(|e| format!("the answer couldn't be read: {}", redact(&e.to_string(), &[])))?;
    if u64::try_from(body.len()).unwrap_or(u64::MAX) > limit {
        if ok {
            return Err(format!("the answer is longer than {} MB", max >> 20));
        }
        body.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    }
    Ok(Reply { status, body, retry_after, request_id })
}

/// The body of `response` as a reader with no limit of ureq's own (10 MB by default): the
/// caller bounds what it reads.
pub(crate) fn body_reader(response: ureq::http::Response<ureq::Body>) -> impl Read + Send + 'static {
    response.into_body().into_with_config().limit(u64::MAX).reader()
}

/// POST a form and read the answer whole.
pub(crate) fn post_form(agent: &ureq::Agent, url: &str, fields: &[(&str, &str)]) -> Result<Reply, String> {
    let response = agent.post(url).header("Accept", "application/json").send_form(fields.iter().copied()).map_err(|e| network(&e))?;
    read(response, MAX_BODY)
}

/// GET JSON (with a bearer token) and read the answer whole.
pub(crate) fn get_json(agent: &ureq::Agent, url: &str, bearer: Option<&str>) -> Result<Reply, String> {
    let mut request = agent.get(url).header("Accept", "application/json");
    if let Some(token) = bearer {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    let response = request.call().map_err(|e| network(&e))?;
    read(response, MAX_BODY)
}

/// A transport error as the person reads it, with nothing secret in it.
pub(crate) fn network(e: &ureq::Error) -> String {
    let why = match e {
        ureq::Error::Timeout(_) => "it took too long to answer".to_string(),
        ureq::Error::HostNotFound => "the server's name wasn't found (is the computer online?)".to_string(),
        ureq::Error::ConnectionFailed => "the connection failed (is the computer online?)".to_string(),
        other => other.to_string(),
    };
    redact(&why, &[])
}

/// Whether `url` is `http://127.0.0.1…` or `http://[::1]…`.
pub(crate) fn is_loopback(url: &str) -> bool {
    let rest = url.strip_prefix("http://").unwrap_or("");
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit_once(':').map_or(host, |(h, port)| if port.chars().all(|c| c.is_ascii_digit()) { h } else { host });
    host == "127.0.0.1" || host == "[::1]"
}

/// `url` is `https://`, or `http://` on this machine.
pub(crate) fn check_url(url: &str) -> Result<(), String> {
    if url.starts_with("https://") || is_loopback(url) {
        Ok(())
    } else {
        Err(format!("`{}` isn't an https address", url.chars().filter(|c| !c.is_control()).take(100).collect::<String>()))
    }
}

/// `s` percent-encoded for a URL query or a form: everything but RFC 3986's unreserved
/// characters.
pub(crate) fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `pairs` as a query string.
pub(crate) fn query(pairs: &[(&str, &str)]) -> String {
    pairs.iter().map(|(k, v)| format!("{}={}", encode(k), encode(v))).collect::<Vec<_>>().join("&")
}

/// Most pairs [`parse_query`] reads.
const MAX_PAIRS: usize = 64;

/// A query string or form body as pairs, decoded (`+` is a space; a bad `%` escape stays as it
/// is; bytes that aren't UTF-8 become U+FFFD). At most [`MAX_PAIRS`] pairs.
pub(crate) fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|p| !p.is_empty())
        .take(MAX_PAIRS)
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (decode(k), decode(v))
        })
        .collect()
}

fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        match b {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' => {
                let hex = bytes.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
                match hex {
                    Some(v) => {
                        out.push(v);
                        i += 3;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            _ => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `text` without the `secrets` in it and without anything that looks like a token (a run of
/// 40 or more token characters), without control characters, and at most [`MAX_MESSAGE`]
/// characters: what a server's or a library's error may say to the person.
pub(crate) fn redact(text: &str, secrets: &[&str]) -> String {
    let mut text = text.to_string();
    for s in secrets.iter().filter(|s| s.len() >= 8) {
        text = text.replace(s, "[redacted]");
    }
    let mut out = String::with_capacity(text.len().min(MAX_MESSAGE * 4));
    let mut run = String::new();
    let token_char = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '=' | '+' | '/' | '~');
    let flush = |run: &mut String, out: &mut String| {
        if run.chars().count() >= 40 {
            out.push_str("[redacted]");
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for c in text.chars() {
        if token_char(c) {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            if !c.is_control() {
                out.push(c);
            } else if c == '\n' || c == '\t' {
                out.push(' ');
            }
        }
    }
    flush(&mut run, &mut out);
    let out = out.trim();
    if out.chars().count() > MAX_MESSAGE { out.chars().take(MAX_MESSAGE).collect::<String>() + "…" } else { out.to_string() }
}

/// Compare two secrets in time that doesn't depend on where they differ.
pub(crate) fn same_secret(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    !b.is_empty() && a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// `n` random bytes from the operating system.
pub(crate) fn random(n: usize) -> Result<Vec<u8>, String> {
    let mut b = vec![0u8; n];
    getrandom::fill(&mut b).map_err(|e| format!("no random numbers: {e}"))?;
    Ok(b)
}

/// The base64url text (no padding) of `bytes`.
pub(crate) fn b64url(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// The bytes of base64url text (padding or not).
pub(crate) fn from_b64url(text: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(text.trim_end_matches('=')).ok()
}

/// Seconds since 1970.
pub(crate) fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_encode_and_decode_and_junk_stays_junk() {
        let q = query(&[("redirect_uri", "http://127.0.0.1:1455/auth/callback"), ("scope", "openid profile"), ("é", "a&b=c")]);
        assert_eq!(q, "redirect_uri=http%3A%2F%2F127.0.0.1%3A1455%2Fauth%2Fcallback&scope=openid%20profile&%C3%A9=a%26b%3Dc");
        let back = parse_query(&q);
        assert_eq!(back[0], ("redirect_uri".to_string(), "http://127.0.0.1:1455/auth/callback".to_string()));
        assert_eq!(back[1].1, "openid profile");
        assert_eq!(back[2], ("é".to_string(), "a&b=c".to_string()));
        assert_eq!(
            parse_query("a=1+2&b=%zz&c=%4&d=%e9&&=x"),
            [
                ("a".to_string(), "1 2".to_string()),
                ("b".to_string(), "%zz".to_string()),
                ("c".to_string(), "%4".to_string()),
                ("d".to_string(), "\u{fffd}".to_string()),
                (String::new(), "x".to_string())
            ]
        );
        assert_eq!(parse_query(&"a=1&".repeat(10_000)).len(), MAX_PAIRS);
        assert_eq!(parse_query("%"), [("%".to_string(), String::new())]);
    }

    #[test]
    fn errors_lose_their_tokens() {
        let secret = "rt-the-refresh-token-1234";
        let shown = redact(&format!("bad token {secret} for eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.c2lnbmF0dXJl\nnext"), &[secret]);
        assert_eq!(shown, "bad token [redacted] for [redacted] next");
        assert!(redact(&"x".repeat(10_000), &[]).len() < 400);
        assert_eq!(redact("short words stay", &[""]), "short words stay");
        assert!(redact(&"é ".repeat(1000), &[]).chars().count() <= 301);
    }

    #[test]
    fn only_https_or_this_machine() {
        assert!(check_url("https://auth.openai.com/api").is_ok());
        assert!(check_url("http://127.0.0.1:9/x").is_ok());
        assert!(check_url("http://[::1]:9/x").is_ok());
        assert!(check_url("http://127.0.0.1").is_ok());
        assert!(check_url("http://auth.openai.com/").is_err());
        assert!(check_url("http://127.0.0.1.evil.example/").is_err());
        assert!(check_url("http://127.0.0.1@evil.example/").is_err());
        assert!(check_url("ftp://127.0.0.1/").is_err());
        assert!(check_url("").is_err());
        assert!(same_secret("abc", "abc") && !same_secret("abc", "abd") && !same_secret("", "") && !same_secret("ab", "abc"));
    }
}

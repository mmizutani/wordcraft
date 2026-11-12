//! Verify local-web ID tokens against the configured issuer's signing keys.

use ring::signature;
use serde_json::Value;
use ureq::http::Uri;

use super::Endpoints;
use super::auth::AuthError;
use super::http::{self, from_b64url};

pub(super) fn verify(token: &str, endpoints: &Endpoints) -> Result<(), AuthError> {
    let fail = || AuthError::Failed("ChatGPT's ID token signature couldn't be verified.".into());
    let (signed, encoded_signature) = token.rsplit_once('.').ok_or_else(fail)?;
    let (encoded_header, _) = signed.split_once('.').ok_or_else(fail)?;
    let header = from_b64url(encoded_header).and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok()).ok_or_else(fail)?;
    let kid = header.get("kid").and_then(Value::as_str).filter(|id| !id.is_empty() && id.len() <= 256).ok_or_else(fail)?;
    if header.get("alg").and_then(Value::as_str) != Some("RS256") || header.get("crit").is_some() {
        return Err(fail());
    }
    let signature = from_b64url(encoded_signature).ok_or_else(fail)?;
    let agent = http::auth_agent(endpoints.is_local());
    let discovery = http::get_json(&agent, &endpoints.discovery, None).map_err(|_| fail())?;
    let configuration = discovery.json();
    if !discovery.ok() || configuration.get("issuer").and_then(Value::as_str) != Some(endpoints.issuer.as_str()) {
        return Err(fail());
    }
    let uri = configuration.get("jwks_uri").and_then(Value::as_str).filter(|url| same_origin(url, &endpoints.discovery)).ok_or_else(fail)?;
    let reply = http::get_json(&agent, uri, None).map_err(|_| fail())?;
    let keys = reply.json();
    let Some(keys) = keys.get("keys").and_then(Value::as_array).filter(|_| reply.ok()) else { return Err(fail()) };
    let mut matches = keys.iter().filter(|key| key.get("kid").and_then(Value::as_str) == Some(kid));
    let key = matches.next().ok_or_else(fail)?;
    if matches.next().is_some()
        || key.get("kty").and_then(Value::as_str) != Some("RSA")
        || key.get("alg").is_some_and(|alg| alg.as_str() != Some("RS256"))
        || key.get("use").is_some_and(|usage| usage.as_str() != Some("sig"))
    {
        return Err(fail());
    }
    let component = |field| key.get(field).and_then(Value::as_str).and_then(from_b64url).ok_or_else(fail);
    let (modulus, exponent) = (component("n")?, component("e")?);
    signature::RsaPublicKeyComponents { n: modulus, e: exponent }
        .verify(&signature::RSA_PKCS1_2048_8192_SHA256, signed.as_bytes(), &signature)
        .map_err(|_| fail())
}

fn same_origin(candidate: &str, trusted: &str) -> bool {
    let (Ok(candidate), Ok(trusted)) = (candidate.parse::<Uri>(), trusted.parse::<Uri>()) else { return false };
    trusted.authority().is_some() && candidate.authority() == trusted.authority() && candidate.scheme() == trusted.scheme()
}

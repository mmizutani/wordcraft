//! Sign in with ChatGPT: the person's ChatGPT plan (Plus or Pro) runs the assistant, with no API
//! key, through OpenAI's open-source token-sharing flow.
//!
//! - [`SignIn`]: one sign-in attempt. It listens on `127.0.0.1` for the browser's answer, gives
//!   the URL the window opens, exchanges the code (PKCE, no client secret) and keeps the
//!   credentials in the settings folder ([`CREDENTIALS_FILE`], mode 0600). The first sign-in
//!   registers WordCraft (`dynamic_agent_client`) and keeps the client id it is issued, with
//!   this installation's host id ([`CLIENT_FILE`]), for every sign-in after it.
//! - [`account`]: who is signed in, read from the file (no network).
//! - [`SignOut`]: deletes the credentials and revokes their refresh token.
//! - [`models`], [`choose_model`]: the models the account may use, and the one chosen.
//! - [`ChatGpt`]: the backend. It calls the Responses API, streamed, with WordCraft's tools as
//!   function tools that run in this process.
//!
//! Tokens never reach the document, the window's state, the control channel or the log, and
//! error text shown to the person has them taken out. Everything that comes from the network
//! or the browser is hostile input: reads are bounded, sizes capped, waits timed out.
//!
//! Desktop only: the web build has no loopback port to sign in with.

mod auth;
mod backend;
mod http;
mod sse;
mod store;

pub use auth::{
    AuthError, CALLBACK_PATH, DEFAULT_PORT, Model, PLAN_SCOPE, SCOPES, SIGN_IN_WAIT, SignIn, SignInOptions, SignOut, SignedOut, choose_model,
    chosen_model, models, models_with, sign_out, sign_out_with,
};
pub use backend::{ChatGpt, MAX_ROUNDS};
pub use store::{Account, CLIENT_FILE, CREDENTIALS_FILE, account};

use std::path::Path;

use crate::offers::{Host, Offer, Readiness};

/// The backend's id, as `assist.backend {id}` names it.
pub const ID: &str = "chatgpt";

/// Its name in the picker, and the name the assistant joins the shared document under.
pub const AUTHOR: &str = "ChatGPT";

/// Where the person sees and manages their plan's usage.
pub const USAGE_PAGE: &str = "https://chatgpt.com/settings/usage";

/// The API the tokens are for (the token requests' `resource`).
pub const RESOURCE: &str = "https://api.openai.com/v1";

/// Where the sign-in and the API are. [`Endpoints::default`] is OpenAI's; tests point them at
/// fake servers on this machine ([`Endpoints::local`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoints {
    /// The issuer, which the id token names.
    pub issuer: String,
    /// The authorization endpoint the browser opens.
    pub authorize: String,
    /// The token endpoint (code exchange and refresh).
    pub token: String,
    /// The OpenID configuration, which names the revocation endpoint.
    pub discovery: String,
    /// The API's base (`…/v1`).
    pub api: String,
    /// The `resource` the tokens are asked for.
    pub resource: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Endpoints {
            issuer: "https://auth.openai.com".into(),
            authorize: "https://auth.openai.com/api/accounts/authorize".into(),
            token: "https://auth.openai.com/api/accounts/oauth/token".into(),
            discovery: "https://auth.openai.com/.well-known/openid-configuration".into(),
            api: RESOURCE.into(),
            resource: RESOURCE.into(),
        }
    }
}

impl Endpoints {
    /// Endpoints on a server at `base` (`http://127.0.0.1:<port>`), for tests: `/authorize`,
    /// `/oauth/token`, `/.well-known/openid-configuration` and `/v1`, with OpenAI's issuer
    /// and resource names.
    pub fn local(base: &str) -> Endpoints {
        let base = base.trim_end_matches('/');
        Endpoints {
            authorize: format!("{base}/authorize"),
            token: format!("{base}/oauth/token"),
            discovery: format!("{base}/.well-known/openid-configuration"),
            api: format!("{base}/v1"),
            ..Endpoints::default()
        }
    }

    /// Whether every endpoint is on this machine (no proxy is used for them).
    pub(crate) fn is_local(&self) -> bool {
        [&self.authorize, &self.token, &self.discovery, &self.api].iter().all(|u| http::is_loopback(u))
    }

    /// Every endpoint is `https://`, or `http://` on this machine (tokens never cross a network
    /// unencrypted).
    pub(crate) fn check(&self) -> Result<(), String> {
        for url in [&self.authorize, &self.token, &self.discovery, &self.api] {
            http::check_url(url)?;
        }
        Ok(())
    }
}

/// Whether ChatGPT can start a conversation on this host: signed in, with plan usage allowed.
pub fn readiness(settings: Option<&Path>) -> Readiness {
    let Some(settings) = settings else {
        return Readiness::Unavailable { why: "WordCraft has no settings folder to keep a sign-in in.".into() };
    };
    match account(settings) {
        None => Readiness::SignIn { how: "Sign in with a ChatGPT Plus or Pro account in Options › Account.".into() },
        Some(a) if !a.plan_usage => Readiness::SignIn {
            how: "ChatGPT plan usage isn't allowed for WordCraft: sign in again in Options › Account and allow it (Plus and Pro).".into(),
        },
        Some(_) => Readiness::Ready,
    }
}

/// ChatGPT as the pane offers it.
pub(crate) fn offer(host: &Host) -> Offer {
    Offer { id: ID.into(), label: AUTHOR.into(), readiness: readiness(host.settings.as_deref()) }
}

#[cfg(test)]
mod backend_tests;
#[cfg(test)]
mod fake_server;
#[cfg(test)]
mod tests;

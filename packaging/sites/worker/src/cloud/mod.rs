mod protocol;

#[cfg(target_arch = "wasm32")]
mod routes;
#[cfg(target_arch = "wasm32")]
mod store;

#[cfg(target_arch = "wasm32")]
pub(super) use routes::{handles, respond};

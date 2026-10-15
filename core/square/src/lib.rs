//! Everything that talks to Square: the one HTTP client and its host
//! allowlist, OAuth PKCE, the keychain token store, and the API calls the app
//! makes, with paging, retries and backoff.

pub mod api;
pub mod http;
pub mod oauth;
pub mod token;

pub use http::{Environment, Error, HttpClient, Method, RetryPolicy, SQUARE_VERSION};
pub use token::{MemoryStore, OsKeychain, StoreError, TokenStore, Tokens};

/// The keychain service name and app-data folder name.
pub const APP_ID: &str = "com.tightline.idlestockfinder";

pub(crate) fn random_u32() -> u32 {
    getrandom::u32().unwrap_or(0)
}

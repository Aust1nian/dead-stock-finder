//! Which Square application and environment this build talks to.

use dsf_square::{Environment, OsKeychain};

/// The production Square application id, set when building a release
/// (`DSF_SQUARE_APP_ID`). Austin creates the application (SQUARE_APP_SETUP.md).
const PRODUCTION_APP_ID: Option<&str> = option_env!("DSF_SQUARE_APP_ID");
/// Development builds with the `sandbox` feature may use Square's Sandbox.
const SANDBOX_APP_ID: Option<&str> = option_env!("DSF_SQUARE_SANDBOX_APP_ID");

pub struct Square {
    pub environment: Environment,
    pub app_id: String,
}

pub fn square() -> Square {
    if cfg!(feature = "sandbox") && std::env::var("DSF_SQUARE_ENV").as_deref() == Ok("sandbox") {
        let app_id = std::env::var("DSF_SQUARE_SANDBOX_APP_ID")
            .ok()
            .or(SANDBOX_APP_ID.map(str::to_string))
            .unwrap_or_default();
        return Square { environment: Environment::Sandbox, app_id };
    }
    Square { environment: Environment::Production, app_id: PRODUCTION_APP_ID.unwrap_or("").to_string() }
}

/// `interactive: false` for background checks: never a keychain prompt.
pub fn keychain(environment: &Environment, interactive: bool) -> OsKeychain {
    OsKeychain::new(dsf_square::APP_ID, environment.name(), interactive)
}

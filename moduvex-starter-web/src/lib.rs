//! # moduvex-starter-web
//!
//! Web application starter for Moduvex. One dependency, zero boilerplate.
//!
//! Bundles: runtime, HTTP server, module system, config, and observability
//! with sensible defaults for web services.
//!
//! ```rust,ignore
//! use moduvex_starter_web::prelude::*;
//!
//! #[moduvex::main]
//! async fn main() {
//!     Moduvex::new().run().await;
//! }
//! ```

// ── Re-exports ──

pub use moduvex_config;
pub use moduvex_core;
pub use moduvex_http;
pub use moduvex_observe;
pub use moduvex_runtime;

// ── Default config ──

/// Embedded default configuration for web applications.
/// User's `app.toml` overrides these values.
pub const WEB_DEFAULTS: &str = r#"
[server]
port = 8080
host = "0.0.0.0"

[observe.log]
level = "info"
format = "pretty"

[observe.metrics]
enabled = true
"#;

/// Create a [`ConfigLoader`] pre-loaded with web defaults.
///
/// Merge order: WEB_DEFAULTS → app.toml → app-{profile}.toml → env vars.
pub fn load_config(
    name: &str,
    dir: &std::path::Path,
) -> Result<moduvex_config::ConfigLoader, moduvex_config::ConfigError> {
    moduvex_config::ConfigLoader::load_with_defaults(WEB_DEFAULTS, name, dir)
}

/// Create a [`ConfigLoader`] from defaults only (no file needed).
pub fn default_config() -> Result<moduvex_config::ConfigLoader, moduvex_config::ConfigError> {
    moduvex_config::ConfigLoader::from_defaults(WEB_DEFAULTS)
}

// ── Prelude ──

pub mod prelude {
    // Core framework
    pub use moduvex_core::prelude::*;

    // HTTP types + extractors
    pub use moduvex_http::{
        FromRequest, HttpServer, IntoHandler, Json, Middleware, Path, Query, Request, Response,
        Router, State, StatusCode,
    };

    // Config
    pub use moduvex_config::{ConfigLoader, Profile};

    // Observability macros
    pub use moduvex_observe::{debug, error, info, trace_event, warn};
    pub use moduvex_observe::{Counter, Gauge, Histogram, Span};

    // Starter helpers
    pub use crate::{default_config, load_config, WEB_DEFAULTS};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_loads_web_defaults() {
        let loader = default_config().unwrap();
        let raw = loader.raw();
        let server = raw.get("server").unwrap().as_table().unwrap();
        assert_eq!(server["port"].as_integer().unwrap(), 8080);
        assert_eq!(server["host"].as_str().unwrap(), "0.0.0.0");
    }

    #[test]
    fn web_defaults_parses_as_valid_toml() {
        // Validates that from_defaults succeeds (proves valid TOML).
        let loader = default_config().unwrap();
        assert!(loader.raw().as_table().is_some());
    }
}

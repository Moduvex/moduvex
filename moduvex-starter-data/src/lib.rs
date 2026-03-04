//! # moduvex-starter-data
//!
//! Data processing starter for Moduvex. Bundles runtime, database (PostgreSQL),
//! module system, and config with sensible pool defaults.
//!
//! ```rust,ignore
//! use moduvex_starter_data::prelude::*;
//!
//! #[moduvex::main]
//! async fn main() {
//!     let config = load_config("app", Path::new(".")).unwrap();
//!     // Pool is configured with sensible defaults
//! }
//! ```

// ── Re-exports ──

pub use moduvex_config;
pub use moduvex_core;
pub use moduvex_db;
pub use moduvex_runtime;

#[cfg(feature = "observe")]
pub use moduvex_observe;

// ── Default config ──

/// Embedded default configuration for data applications.
pub const DATA_DEFAULTS: &str = r#"
[pool]
max_connections = 10
min_idle = 2
connect_timeout_secs = 30
idle_timeout_secs = 600

[observe.log]
level = "info"
format = "pretty"
"#;

/// Create a [`ConfigLoader`] pre-loaded with data defaults.
pub fn load_config(
    name: &str,
    dir: &std::path::Path,
) -> Result<moduvex_config::ConfigLoader, moduvex_config::ConfigError> {
    moduvex_config::ConfigLoader::load_with_defaults(DATA_DEFAULTS, name, dir)
}

/// Create a [`ConfigLoader`] from data defaults only (no file needed).
pub fn default_config() -> Result<moduvex_config::ConfigLoader, moduvex_config::ConfigError> {
    moduvex_config::ConfigLoader::from_defaults(DATA_DEFAULTS)
}

// ── Prelude ──

pub mod prelude {
    // Core framework
    pub use moduvex_core::prelude::*;

    // Database types
    pub use moduvex_db::{ConnectionPool, PoolConfig, Row, RowSet, Transaction};

    // Config
    pub use moduvex_config::{ConfigLoader, Profile};

    // Starter helpers
    pub use crate::{default_config, load_config, DATA_DEFAULTS};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_loads_data_defaults() {
        let loader = default_config().unwrap();
        let raw = loader.raw();
        let pool = raw.get("pool").unwrap().as_table().unwrap();
        assert_eq!(pool["max_connections"].as_integer().unwrap(), 10);
        assert_eq!(pool["min_idle"].as_integer().unwrap(), 2);
    }

    #[test]
    fn data_defaults_parses_as_valid_toml() {
        let loader = default_config().unwrap();
        assert!(loader.raw().as_table().is_some());
    }
}

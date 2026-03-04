//! `moduvex-config` — Typed TOML config with per-module scoping.
//!
//! Loads `{name}.toml`, overlays `{name}-{profile}.toml`, merges
//! `MODUVEX__*` env vars, then scopes sections into typed structs.
//!
//! # Example
//! ```rust,no_run
//! use moduvex_config::{ConfigLoader, Profile};
//! use serde::Deserialize;
//!
//! #[derive(Deserialize)]
//! struct ServerConfig { port: u16 }
//!
//! let loader = ConfigLoader::load("app", std::path::Path::new(".")).unwrap();
//! let server: std::sync::Arc<ServerConfig> = loader.scope("server").unwrap();
//! ```

pub mod error;
pub mod loader;
pub mod merge;
pub mod profile;
pub mod scope;
pub mod validate;

pub use error::ConfigError;
pub use profile::Profile;
pub use validate::{Validate, ValidationError};

use std::path::Path;
use std::sync::Arc;
use serde::de::DeserializeOwned;

/// Main config loader. Holds the merged TOML tree after loading.
#[derive(Debug, Clone)]
pub struct ConfigLoader {
    root: toml::Value,
    profile: Profile,
}

impl ConfigLoader {
    /// Load config from `{dir}/{name}.toml` with profile overlay and env merging.
    ///
    /// 1. Detect profile from `MODUVEX_PROFILE` env var (default: dev)
    /// 2. Load `{name}.toml` (required)
    /// 3. Merge `{name}-{profile}.toml` if exists
    /// 4. Merge `MODUVEX__*` env var overrides
    pub fn load(name: &str, dir: &Path) -> Result<Self, ConfigError> {
        let profile = Profile::from_env();
        Self::load_with_profile(name, dir, profile)
    }

    /// Load with an explicit profile (useful for testing).
    pub fn load_with_profile(
        name: &str,
        dir: &Path,
        profile: Profile,
    ) -> Result<Self, ConfigError> {
        let toml_val = loader::load_toml(dir, name, &profile)?;
        let merged = merge::merge_env_overrides(toml_val);
        Ok(Self { root: merged, profile })
    }

    /// Create a ConfigLoader from embedded TOML defaults only (no file).
    ///
    /// Useful for starters that provide sensible defaults without requiring
    /// an `app.toml` file. Env var overrides are still applied.
    pub fn from_defaults(defaults: &str) -> Result<Self, ConfigError> {
        let base: toml::Value = defaults.parse().map_err(|e: toml::de::Error| {
            ConfigError::Parse { path: "<defaults>".into(), source: e.to_string() }
        })?;
        let merged = merge::merge_env_overrides(base);
        Ok(Self { root: merged, profile: Profile::from_env() })
    }

    /// Load config with embedded defaults as the lowest-priority layer.
    ///
    /// Merge order: defaults → file → profile overlay → env vars.
    pub fn load_with_defaults(
        defaults: &str,
        name: &str,
        dir: &Path,
    ) -> Result<Self, ConfigError> {
        let default_val: toml::Value = defaults.parse().map_err(|e: toml::de::Error| {
            ConfigError::Parse { path: "<defaults>".into(), source: e.to_string() }
        })?;
        let profile = Profile::from_env();
        // Try loading file; if missing, just use defaults
        let file_val = loader::load_toml(dir, name, &profile);
        let base = match file_val {
            Ok(file) => loader::deep_merge(default_val, file),
            Err(_) => default_val,
        };
        let merged = merge::merge_env_overrides(base);
        Ok(Self { root: merged, profile })
    }

    /// Extract a config section and deserialize into `T`.
    ///
    /// The section key corresponds to a TOML table name (e.g. `"server"`
    /// for `[server]`).
    pub fn scope<T: DeserializeOwned>(&self, section: &str) -> Result<Arc<T>, ConfigError> {
        scope::extract_section(&self.root, section)
    }

    /// Returns the active profile.
    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    /// Returns a reference to the raw merged TOML value tree.
    pub fn raw(&self) -> &toml::Value {
        &self.root
    }
}

/// Trait for modules to declare their config section.
///
/// Implement this on your module struct so the framework can
/// automatically load and scope config during boot.
pub trait ModuleConfig {
    /// The typed config struct (must be `Deserialize`).
    type Config: DeserializeOwned + Send + Sync + 'static;
    /// The TOML section key (e.g. `"user"` for `[user]`).
    fn config_prefix() -> &'static str;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::fs;

    #[derive(Debug, Deserialize, PartialEq)]
    struct AppConfig {
        name: String,
        debug: bool,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct DbConfig {
        host: String,
        port: u16,
    }

    fn write_test_config(dir: &Path) {
        fs::write(
            dir.join("app.toml"),
            "[app]\nname = \"test\"\ndebug = true\n\n[db]\nhost = \"localhost\"\nport = 5432\n",
        )
        .unwrap();
    }

    #[test]
    fn load_and_scope_sections() {
        let dir = std::env::temp_dir().join(format!("moduvex-cfg-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        write_test_config(&dir);

        let loader = ConfigLoader::load_with_profile("app", &dir, Profile::Dev).unwrap();
        let app: Arc<AppConfig> = loader.scope("app").unwrap();
        let db: Arc<DbConfig> = loader.scope("db").unwrap();

        assert_eq!(app.name, "test");
        assert!(app.debug);
        assert_eq!(db.host, "localhost");
        assert_eq!(db.port, 5432);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn profile_defaults_to_dev() {
        std::env::remove_var("MODUVEX_PROFILE");
        let p = Profile::from_env();
        assert_eq!(p, Profile::Dev);
    }

    #[test]
    fn missing_section_error() {
        let dir = std::env::temp_dir().join(format!("moduvex-cfg-miss-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("app.toml"), "[other]\nkey = 1\n").unwrap();

        let loader = ConfigLoader::load_with_profile("app", &dir, Profile::Dev).unwrap();
        let result = loader.scope::<AppConfig>("app");
        assert!(matches!(result, Err(ConfigError::MissingSection { .. })));

        fs::remove_dir_all(&dir).ok();
    }
}

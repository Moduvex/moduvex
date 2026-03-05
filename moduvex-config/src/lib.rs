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

use serde::de::DeserializeOwned;
use std::path::Path;
use std::sync::Arc;

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
        Ok(Self {
            root: merged,
            profile,
        })
    }

    /// Create a ConfigLoader from embedded TOML defaults only (no file).
    ///
    /// Useful for starters that provide sensible defaults without requiring
    /// an `app.toml` file. Env var overrides are still applied.
    pub fn from_defaults(defaults: &str) -> Result<Self, ConfigError> {
        let base: toml::Value =
            toml::from_str(defaults).map_err(|e: toml::de::Error| ConfigError::Parse {
                path: "<defaults>".into(),
                source: e.to_string(),
            })?;
        let merged = merge::merge_env_overrides(base);
        Ok(Self {
            root: merged,
            profile: Profile::from_env(),
        })
    }

    /// Load config with embedded defaults as the lowest-priority layer.
    ///
    /// Merge order: defaults → file → profile overlay → env vars.
    pub fn load_with_defaults(defaults: &str, name: &str, dir: &Path) -> Result<Self, ConfigError> {
        let default_val: toml::Value =
            toml::from_str(defaults).map_err(|e: toml::de::Error| ConfigError::Parse {
                path: "<defaults>".into(),
                source: e.to_string(),
            })?;
        let profile = Profile::from_env();
        // Try loading file; if missing, just use defaults
        let file_val = loader::load_toml(dir, name, &profile);
        let base = match file_val {
            Ok(file) => loader::deep_merge(default_val, file),
            Err(_) => default_val,
        };
        let merged = merge::merge_env_overrides(base);
        Ok(Self {
            root: merged,
            profile,
        })
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

    #[test]
    fn from_defaults_loads_toml_string() {
        let defaults = "[server]\nhost = \"0.0.0.0\"\nport = 8080\n";
        let loader = ConfigLoader::from_defaults(defaults).unwrap();

        #[derive(Deserialize)]
        struct ServerCfg { host: String, port: u16 }

        let srv: std::sync::Arc<ServerCfg> = loader.scope("server").unwrap();
        assert_eq!(srv.host, "0.0.0.0");
        assert_eq!(srv.port, 8080);
    }

    #[test]
    fn from_defaults_invalid_toml_returns_error() {
        let result = ConfigLoader::from_defaults("[[[ not valid toml");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ConfigError::Parse { .. }));
    }

    #[test]
    fn from_defaults_empty_string_creates_empty_loader() {
        let loader = ConfigLoader::from_defaults("").unwrap();
        // Empty TOML is a valid empty table
        let result = loader.scope::<AppConfig>("app");
        assert!(matches!(result, Err(ConfigError::MissingSection { .. })));
    }

    #[test]
    fn profile_getter_returns_correct_profile() {
        let dir = std::env::temp_dir().join(format!("moduvex-cfg-prof-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("app.toml"), "[app]\nname = \"test\"\ndebug = false\n").unwrap();

        let loader = ConfigLoader::load_with_profile("app", &dir, Profile::Prod).unwrap();
        assert_eq!(loader.profile(), &Profile::Prod);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn raw_returns_merged_toml_value() {
        let defaults = "[server]\nport = 3000\n";
        let loader = ConfigLoader::from_defaults(defaults).unwrap();
        let raw = loader.raw();
        assert!(raw.get("server").is_some());
        assert_eq!(raw["server"]["port"].as_integer().unwrap(), 3000);
    }

    #[test]
    fn load_with_defaults_uses_defaults_when_file_missing() {
        let defaults = "[app]\nname = \"default-name\"\ndebug = false\n";
        // Point to a dir with no files
        let dir = std::env::temp_dir().join(format!("moduvex-cfg-nofile-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();

        let loader = ConfigLoader::load_with_defaults(defaults, "app", &dir).unwrap();
        let app: std::sync::Arc<AppConfig> = loader.scope("app").unwrap();
        assert_eq!(app.name, "default-name");
        assert!(!app.debug);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_with_defaults_file_overrides_defaults() {
        let defaults = "[app]\nname = \"default-name\"\ndebug = false\n";
        let dir = std::env::temp_dir().join(format!("moduvex-cfg-override-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        // File overrides the name field
        fs::write(dir.join("app.toml"), "[app]\nname = \"file-name\"\ndebug = true\n").unwrap();

        let loader = ConfigLoader::load_with_defaults(defaults, "app", &dir).unwrap();
        let app: std::sync::Arc<AppConfig> = loader.scope("app").unwrap();
        assert_eq!(app.name, "file-name");
        assert!(app.debug);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn profile_overlay_overrides_base_section() {
        let dir = std::env::temp_dir().join(format!("moduvex-cfg-ov-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        // Base file
        fs::write(
            dir.join("app.toml"),
            "[app]\nname = \"base\"\ndebug = false\n\n[db]\nhost = \"localhost\"\nport = 5432\n",
        ).unwrap();
        // Prod overlay — overrides debug
        fs::write(
            dir.join("app-prod.toml"),
            "[app]\ndebug = true\n",
        ).unwrap();

        let loader = ConfigLoader::load_with_profile("app", &dir, Profile::Prod).unwrap();
        let app: std::sync::Arc<AppConfig> = loader.scope("app").unwrap();
        // Name is preserved from base, debug is overridden
        assert_eq!(app.name, "base");
        assert!(app.debug);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_base_file_returns_error() {
        let dir = std::env::temp_dir().join(format!("moduvex-cfg-nob-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();

        let result = ConfigLoader::load_with_profile("nonexistent", &dir, Profile::Dev);
        assert!(result.is_err());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scope_deserialize_error_on_wrong_type() {
        let dir = std::env::temp_dir().join(format!("moduvex-cfg-badtype-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        // port should be u16 but file has a string
        fs::write(dir.join("app.toml"), "[db]\nhost = \"localhost\"\nport = \"bad\"\n").unwrap();

        let loader = ConfigLoader::load_with_profile("app", &dir, Profile::Dev).unwrap();
        let result = loader.scope::<DbConfig>("db");
        assert!(matches!(result, Err(ConfigError::Deserialize { .. })));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn env_var_override_applied_in_from_defaults() {
        std::env::set_var("MODUVEX__ENVTEST__PORT", "9999");
        let defaults = "[envtest]\nport = 1234\n";
        let loader = ConfigLoader::from_defaults(defaults).unwrap();
        let raw = loader.raw();
        // Env var should override the default
        assert_eq!(raw["envtest"]["port"].as_integer().unwrap(), 9999);
        std::env::remove_var("MODUVEX__ENVTEST__PORT");
    }

    #[test]
    fn unicode_string_in_config() {
        let defaults = "[app]\nname = \"Привет мир\"\ndebug = false\n";
        let loader = ConfigLoader::from_defaults(defaults).unwrap();
        let app: std::sync::Arc<AppConfig> = loader.scope("app").unwrap();
        assert_eq!(app.name, "Привет мир");
    }

    #[test]
    fn very_long_string_in_config() {
        let long_name = "a".repeat(1000);
        let toml_str = format!("[app]\nname = \"{}\"\ndebug = false\n", long_name);
        let loader = ConfigLoader::from_defaults(&toml_str).unwrap();
        let app: std::sync::Arc<AppConfig> = loader.scope("app").unwrap();
        assert_eq!(app.name.len(), 1000);
    }

    #[test]
    fn special_chars_in_config_string() {
        // TOML string with special characters (escaped backslash and quote)
        let defaults = "[app]\nname = \"test\\u0041\"\ndebug = true\n";
        let loader = ConfigLoader::from_defaults(defaults).unwrap();
        let app: std::sync::Arc<AppConfig> = loader.scope("app").unwrap();
        // \u0041 is 'A'
        assert_eq!(app.name, "testA");
    }

    #[test]
    fn loader_clone_shares_same_data() {
        let defaults = "[app]\nname = \"clone-test\"\ndebug = false\n";
        let loader = ConfigLoader::from_defaults(defaults).unwrap();
        let loader2 = loader.clone();
        let app1: std::sync::Arc<AppConfig> = loader.scope("app").unwrap();
        let app2: std::sync::Arc<AppConfig> = loader2.scope("app").unwrap();
        assert_eq!(app1.name, app2.name);
    }
}

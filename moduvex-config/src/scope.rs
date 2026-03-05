//! Per-module config section extraction and deserialization.
//!
//! Extracts a TOML section by key, deserializes into the target type,
//! and wraps in `Arc<T>` for DI injection.

use crate::error::ConfigError;
use serde::de::DeserializeOwned;
use std::sync::Arc;

/// Extract a section from the root TOML value and deserialize into `T`.
///
/// Returns `Arc<T>` ready for insertion into AppContext.
pub fn extract_section<T: DeserializeOwned>(
    root: &toml::Value,
    section: &str,
) -> Result<Arc<T>, ConfigError> {
    let section_val = root
        .get(section)
        .ok_or_else(|| ConfigError::MissingSection {
            section: section.to_string(),
        })?;

    let typed: T = section_val
        .clone()
        .try_into()
        .map_err(|e: toml::de::Error| ConfigError::Deserialize {
            section: section.to_string(),
            source: e.to_string(),
        })?;

    Ok(Arc::new(typed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    struct ServerConfig {
        port: u16,
        host: String,
    }

    #[test]
    fn extract_valid_section() {
        let root: toml::Value =
            toml::from_str("[server]\nport = 8080\nhost = \"localhost\"\n").unwrap();
        let cfg: Arc<ServerConfig> = extract_section(&root, "server").unwrap();
        assert_eq!(cfg.port, 8080);
        assert_eq!(cfg.host, "localhost");
    }

    #[test]
    fn missing_section_returns_error() {
        let root: toml::Value = toml::from_str("[other]\nkey = 1\n").unwrap();
        let result = extract_section::<ServerConfig>(&root, "server");
        assert!(matches!(result, Err(ConfigError::MissingSection { .. })));
    }

    #[test]
    fn wrong_type_returns_deserialize_error() {
        let root: toml::Value = toml::from_str("[server]\nport = \"not_a_number\"\n").unwrap();
        let result = extract_section::<ServerConfig>(&root, "server");
        assert!(matches!(result, Err(ConfigError::Deserialize { .. })));
    }

    #[test]
    fn extract_section_arc_points_to_correct_values() {
        let root: toml::Value =
            toml::from_str("[server]\nport = 443\nhost = \"example.com\"\n").unwrap();
        let cfg: Arc<ServerConfig> = extract_section(&root, "server").unwrap();
        assert_eq!(cfg.port, 443);
        assert_eq!(cfg.host, "example.com");
    }

    #[test]
    fn missing_section_error_contains_section_name() {
        let root: toml::Value = toml::from_str("[other]\nkey = 1\n").unwrap();
        let result = extract_section::<ServerConfig>(&root, "database");
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("database"));
    }

    #[test]
    fn deserialize_error_contains_section_name() {
        let root: toml::Value =
            toml::from_str("[server]\nport = \"bad\"\nhost = 123\n").unwrap();
        let result = extract_section::<ServerConfig>(&root, "server");
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("server"));
    }

    #[test]
    fn extract_nested_struct() {
        #[derive(Debug, serde::Deserialize, PartialEq)]
        struct DbConfig {
            url: String,
            pool_size: u32,
        }

        let root: toml::Value =
            toml::from_str("[db]\nurl = \"postgres://localhost/mydb\"\npool_size = 10\n").unwrap();
        let cfg: Arc<DbConfig> = extract_section(&root, "db").unwrap();
        assert_eq!(cfg.url, "postgres://localhost/mydb");
        assert_eq!(cfg.pool_size, 10);
    }

    #[test]
    fn extract_string_only_section() {
        #[derive(Debug, serde::Deserialize)]
        struct Simple {
            value: String,
        }

        let root: toml::Value = toml::from_str("[sec]\nvalue = \"hello\"\n").unwrap();
        let cfg: Arc<Simple> = extract_section(&root, "sec").unwrap();
        assert_eq!(cfg.value, "hello");
    }
}

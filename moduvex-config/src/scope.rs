//! Per-module config section extraction and deserialization.
//!
//! Extracts a TOML section by key, deserializes into the target type,
//! and wraps in `Arc<T>` for DI injection.

use std::sync::Arc;
use serde::de::DeserializeOwned;
use crate::error::ConfigError;

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

    let typed: T = section_val.clone().try_into().map_err(|e: toml::de::Error| {
        ConfigError::Deserialize {
            section: section.to_string(),
            source: e.to_string(),
        }
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
            "[server]\nport = 8080\nhost = \"localhost\"\n".parse().unwrap();
        let cfg: Arc<ServerConfig> = extract_section(&root, "server").unwrap();
        assert_eq!(cfg.port, 8080);
        assert_eq!(cfg.host, "localhost");
    }

    #[test]
    fn missing_section_returns_error() {
        let root: toml::Value = "[other]\nkey = 1\n".parse().unwrap();
        let result = extract_section::<ServerConfig>(&root, "server");
        assert!(matches!(result, Err(ConfigError::MissingSection { .. })));
    }

    #[test]
    fn wrong_type_returns_deserialize_error() {
        let root: toml::Value = "[server]\nport = \"not_a_number\"\n".parse().unwrap();
        let result = extract_section::<ServerConfig>(&root, "server");
        assert!(matches!(result, Err(ConfigError::Deserialize { .. })));
    }
}

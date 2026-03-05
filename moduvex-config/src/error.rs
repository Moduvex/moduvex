//! Config error types with human-readable messages.

use crate::validate::ValidationError;
use std::fmt;

/// All possible config errors.
#[derive(Debug)]
pub enum ConfigError {
    /// Failed to read config file from disk.
    FileRead { path: String, source: String },
    /// Failed to parse TOML syntax.
    Parse { path: String, source: String },
    /// Missing required config section for a module.
    MissingSection { section: String },
    /// Failed to deserialize a section into the target type.
    Deserialize { section: String, source: String },
    /// Validation rules failed after deserialization.
    Validation {
        section: String,
        errors: Vec<ValidationError>,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FileRead { path, source } => {
                write!(f, "cannot read config file '{}': {}", path, source)
            }
            Self::Parse { path, source } => {
                write!(f, "TOML parse error in '{}': {}", path, source)
            }
            Self::MissingSection { section } => {
                write!(f, "missing config section [{}]", section)
            }
            Self::Deserialize { section, source } => {
                write!(f, "config [{}] deserialization failed: {}", section, source)
            }
            Self::Validation { section, errors } => {
                write!(f, "config [{}] validation failed:", section)?;
                for e in errors {
                    write!(f, "\n  - {}", e)?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_file_read() {
        let e = ConfigError::FileRead {
            path: "app.toml".into(),
            source: "not found".into(),
        };
        assert!(e.to_string().contains("app.toml"));
    }

    #[test]
    fn display_missing_section() {
        let e = ConfigError::MissingSection {
            section: "db".into(),
        };
        assert_eq!(e.to_string(), "missing config section [db]");
    }

    #[test]
    fn display_validation_errors() {
        let e = ConfigError::Validation {
            section: "server".into(),
            errors: vec![ValidationError {
                field: "port".into(),
                message: "must be > 0".into(),
            }],
        };
        let s = e.to_string();
        assert!(s.contains("[server]"));
        assert!(s.contains("port"));
    }

    #[test]
    fn display_parse_error() {
        let e = ConfigError::Parse {
            path: "config.toml".into(),
            source: "unexpected character".into(),
        };
        let s = e.to_string();
        assert!(s.contains("config.toml"));
        assert!(s.contains("unexpected character"));
    }

    #[test]
    fn display_deserialize_error() {
        let e = ConfigError::Deserialize {
            section: "db".into(),
            source: "invalid type: expected u16".into(),
        };
        let s = e.to_string();
        assert!(s.contains("[db]"));
        assert!(s.contains("invalid type"));
    }

    #[test]
    fn config_error_is_std_error() {
        // Verify ConfigError implements std::error::Error (source returns None)
        let e = ConfigError::MissingSection {
            section: "missing".into(),
        };
        let err: &dyn std::error::Error = &e;
        // source() is not overridden, so it returns None by default
        assert!(err.source().is_none());
    }

    #[test]
    fn validation_error_with_multiple_fields() {
        let e = ConfigError::Validation {
            section: "app".into(),
            errors: vec![
                ValidationError {
                    field: "host".into(),
                    message: "cannot be empty".into(),
                },
                ValidationError {
                    field: "port".into(),
                    message: "must be in range 1-65535".into(),
                },
            ],
        };
        let s = e.to_string();
        assert!(s.contains("host"));
        assert!(s.contains("port"));
        assert!(s.contains("cannot be empty"));
        assert!(s.contains("must be in range"));
    }

    #[test]
    fn file_read_error_contains_path_and_source() {
        let e = ConfigError::FileRead {
            path: "/etc/moduvex/app.toml".into(),
            source: "permission denied".into(),
        };
        let s = e.to_string();
        assert!(s.contains("/etc/moduvex/app.toml"));
        assert!(s.contains("permission denied"));
    }

    #[test]
    fn missing_section_contains_section_name() {
        let e = ConfigError::MissingSection {
            section: "redis".into(),
        };
        assert!(e.to_string().contains("redis"));
    }
}

//! Config error types with human-readable messages.

use std::fmt;
use crate::validate::ValidationError;

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
    Validation { section: String, errors: Vec<ValidationError> },
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
        let e = ConfigError::MissingSection { section: "db".into() };
        assert_eq!(e.to_string(), "missing config section [db]");
    }

    #[test]
    fn display_validation_errors() {
        let e = ConfigError::Validation {
            section: "server".into(),
            errors: vec![
                ValidationError { field: "port".into(), message: "must be > 0".into() },
            ],
        };
        let s = e.to_string();
        assert!(s.contains("[server]"));
        assert!(s.contains("port"));
    }
}

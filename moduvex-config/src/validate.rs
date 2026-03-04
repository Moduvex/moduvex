//! Validation trait for config structs.
//!
//! Modules implement `Validate` on their config types to run
//! domain-specific checks after deserialization.

use std::fmt;

/// Error from config validation.
#[derive(Debug, Clone)]
pub struct ValidationError {
    pub field: String,
    pub message: String,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "config validation: {}: {}", self.field, self.message)
    }
}

impl std::error::Error for ValidationError {}

/// Trait for config structs to implement custom validation.
///
/// Called after deserialization. Return all errors (don't short-circuit)
/// so the user sees every problem at once.
///
/// Types that don't need validation can skip implementing this trait;
/// `ConfigLoader::scope()` will only validate when explicitly requested.
pub trait Validate {
    fn validate(&self) -> Result<(), Vec<ValidationError>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestConfig {
        port: u16,
    }

    impl Validate for TestConfig {
        fn validate(&self) -> Result<(), Vec<ValidationError>> {
            let mut errors = Vec::new();
            if self.port == 0 {
                errors.push(ValidationError {
                    field: "port".into(),
                    message: "must be > 0".into(),
                });
            }
            if errors.is_empty() {
                Ok(())
            } else {
                Err(errors)
            }
        }
    }

    #[test]
    fn valid_config_passes() {
        let cfg = TestConfig { port: 8080 };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn invalid_config_collects_errors() {
        let cfg = TestConfig { port: 0 };
        let errs = cfg.validate().unwrap_err();
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].field, "port");
    }

    #[test]
    fn validation_error_display() {
        let e = ValidationError {
            field: "host".into(),
            message: "cannot be empty".into(),
        };
        assert_eq!(e.to_string(), "config validation: host: cannot be empty");
    }
}

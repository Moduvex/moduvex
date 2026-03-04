//! Error classification traits — Domain, Infra, Config categories.
//!
//! These traits allow errors to carry semantic metadata (HTTP status, retryability)
//! without coupling the core to any specific HTTP framework.

use std::fmt;

// ── DomainError ───────────────────────────────────────────────────────────────

/// A business-logic error with a stable error code and HTTP status mapping.
///
/// Implement this trait on your domain error types (validation failures,
/// business rule violations, not-found errors, etc.).
pub trait DomainError: std::error::Error + Send + Sync + 'static {
    /// Stable, machine-readable error code (e.g. `"user.not_found"`).
    fn error_code(&self) -> &str;

    /// HTTP status code that most naturally maps to this error (e.g. 404, 422).
    fn http_status(&self) -> u16;

    /// Whether this error should be exposed verbatim to external callers.
    /// Defaults to `true` for domain errors (they are intentional & safe to expose).
    fn is_public(&self) -> bool {
        true
    }
}

// ── InfraError ────────────────────────────────────────────────────────────────

/// An infrastructure / system-level error (DB, network, I/O, etc.).
pub trait InfraError: std::error::Error + Send + Sync + 'static {
    /// Whether the failing operation is safe to retry automatically.
    fn is_retryable(&self) -> bool;

    /// Suggested retry delay in milliseconds, if any.
    fn retry_after_ms(&self) -> Option<u64> {
        None
    }
}

// ── ConfigError ───────────────────────────────────────────────────────────────

/// Configuration / validation error produced during the Config or Validate phase.
#[derive(Debug)]
pub struct ConfigError {
    /// Human-readable explanation of what is wrong.
    pub message: String,
    /// Dot-separated config key that is invalid, if applicable.
    pub key: Option<String>,
}

impl ConfigError {
    /// Create a new config error with a message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            key: None,
        }
    }

    /// Attach the config key that is invalid.
    pub fn with_key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.key {
            Some(k) => write!(f, "config error at '{}': {}", k, self.message),
            None => write!(f, "config error: {}", self.message),
        }
    }
}

impl std::error::Error for ConfigError {}

// ── LifecycleError ────────────────────────────────────────────────────────────

/// Error produced during lifecycle phase transitions.
#[derive(Debug)]
pub struct LifecycleError {
    pub message: String,
    /// Name of the module that caused the failure, if known.
    pub module: Option<&'static str>,
}

impl LifecycleError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            module: None,
        }
    }

    pub fn in_module(mut self, module: &'static str) -> Self {
        self.module = Some(module);
        self
    }
}

impl fmt::Display for LifecycleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.module {
            Some(m) => write!(f, "lifecycle error in module '{}': {}", m, self.message),
            None => write!(f, "lifecycle error: {}", self.message),
        }
    }
}

impl std::error::Error for LifecycleError {}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct NotFound(String);
    impl fmt::Display for NotFound {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "not found: {}", self.0)
        }
    }
    impl std::error::Error for NotFound {}
    impl DomainError for NotFound {
        fn error_code(&self) -> &str {
            "resource.not_found"
        }
        fn http_status(&self) -> u16 {
            404
        }
    }

    #[test]
    fn domain_error_fields() {
        let e = NotFound("user 42".into());
        assert_eq!(e.error_code(), "resource.not_found");
        assert_eq!(e.http_status(), 404);
        assert!(e.is_public());
    }

    #[test]
    fn config_error_display_with_key() {
        let e = ConfigError::new("must be positive").with_key("server.port");
        assert!(e.to_string().contains("server.port"));
        assert!(e.to_string().contains("must be positive"));
    }

    #[test]
    fn lifecycle_error_display_with_module() {
        let e = LifecycleError::new("startup failed").in_module("UserModule");
        assert!(e.to_string().contains("UserModule"));
    }
}

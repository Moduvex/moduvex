//! Error context chaining — attach human-readable context to any error.
//!
//! Provides the `Context` extension trait so callers can write:
//! ```rust,ignore
//! result.context("while starting UserModule")?;
//! ```

use std::fmt;

use crate::error::ModuvexError;

// ── ContextError wrapper ──────────────────────────────────────────────────────

/// An error with attached context message.
#[derive(Debug)]
pub struct ContextError {
    context: String,
    source: Box<dyn std::error::Error + Send + Sync + 'static>,
}

impl fmt::Display for ContextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.context, self.source)
    }
}

impl std::error::Error for ContextError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

// ── Context extension trait ───────────────────────────────────────────────────

/// Extension trait for adding context to `Result<T, ModuvexError>`.
pub trait Context<T> {
    /// Wrap the error (if any) with a static context string.
    fn context(self, msg: &'static str) -> Result<T, ModuvexError>;

    /// Wrap the error with a lazily-computed context string.
    fn with_context<F: FnOnce() -> String>(self, f: F) -> Result<T, ModuvexError>;
}

impl<T> Context<T> for Result<T, ModuvexError> {
    fn context(self, msg: &'static str) -> Result<T, ModuvexError> {
        self.map_err(|e| {
            let boxed: Box<dyn std::error::Error + Send + Sync> =
                Box::new(ContextError { context: msg.to_string(), source: Box::new(e) });
            ModuvexError::Other(boxed)
        })
    }

    fn with_context<F: FnOnce() -> String>(self, f: F) -> Result<T, ModuvexError> {
        self.map_err(|e| {
            let boxed: Box<dyn std::error::Error + Send + Sync> =
                Box::new(ContextError { context: f(), source: Box::new(e) });
            ModuvexError::Other(boxed)
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{ModuvexError, classify::ConfigError};

    fn failing() -> Result<(), ModuvexError> {
        Err(ModuvexError::Config(ConfigError::new("bad value")))
    }

    #[test]
    fn static_context_wraps_error() {
        let result = failing().context("while loading config");
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("while loading config"), "got: {msg}");
    }

    #[test]
    fn dynamic_context_wraps_error() {
        let name = "UserModule";
        let result = failing().with_context(|| format!("while starting {name}"));
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("UserModule"), "got: {msg}");
    }
}

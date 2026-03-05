//! Error context chaining — attach human-readable context to any error.
//!
//! Provides the `Context` extension trait so callers can write:
//! ```rust,ignore
//! result.context("while starting UserModule")?;
//! ```
//!
//! # Classification preservation
//!
//! `.context()` and `.with_context()` preserve the original `ModuvexError`
//! variant. Adding context to an `Infra` error keeps it `Infra`; adding
//! context to a `Domain` error keeps it `Domain`, etc.
//!
//! The context message is stored in a `ContextError` wrapper that implements
//! `Display` as `"<context>: <original>"`. The wrapper is stored as the
//! `source` inside a newtype that re-implements the appropriate trait
//! (`InfraError`, `DomainError`, etc.) so the variant stays correct.

use std::fmt;

use crate::error::classify::{DomainError, InfraError};
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

// ── Classification-preserving wrapper types ───────────────────────────────────

/// Wraps a `ContextError` and re-implements `InfraError` so the `Infra`
/// variant is preserved when context is added to an infra error.
#[derive(Debug)]
struct InfraContextError {
    inner: ContextError,
    retryable: bool,
    retry_after_ms: Option<u64>,
}

impl fmt::Display for InfraContextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.inner, f)
    }
}

impl std::error::Error for InfraContextError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.inner.source()
    }
}

impl InfraError for InfraContextError {
    fn is_retryable(&self) -> bool {
        self.retryable
    }
    fn retry_after_ms(&self) -> Option<u64> {
        self.retry_after_ms
    }
}

/// Wraps a `ContextError` and re-implements `DomainError` so the `Domain`
/// variant is preserved when context is added to a domain error.
#[derive(Debug)]
struct DomainContextError {
    inner: ContextError,
    error_code: String,
    http_status: u16,
    is_public: bool,
}

impl fmt::Display for DomainContextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.inner, f)
    }
}

impl std::error::Error for DomainContextError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.inner.source()
    }
}

impl DomainError for DomainContextError {
    fn error_code(&self) -> &str {
        &self.error_code
    }
    fn http_status(&self) -> u16 {
        self.http_status
    }
    fn is_public(&self) -> bool {
        self.is_public
    }
}

// ── Internal helper ───────────────────────────────────────────────────────────

/// Wrap `original` with `context_msg`, preserving the `ModuvexError` variant.
fn wrap_with_context(original: ModuvexError, context_msg: String) -> ModuvexError {
    match original {
        ModuvexError::Infra(ref e) => {
            // Capture retryability metadata before consuming `original`.
            let retryable = e.is_retryable();
            let retry_ms = e.retry_after_ms();
            let wrapper = InfraContextError {
                inner: ContextError {
                    context: context_msg,
                    source: Box::new(original),
                },
                retryable,
                retry_after_ms: retry_ms,
            };
            ModuvexError::Infra(Box::new(wrapper))
        }
        ModuvexError::Domain(ref e) => {
            // Capture domain metadata before consuming `original`.
            let code = e.error_code().to_owned();
            let status = e.http_status();
            let public = e.is_public();
            let wrapper = DomainContextError {
                inner: ContextError {
                    context: context_msg,
                    source: Box::new(original),
                },
                error_code: code,
                http_status: status,
                is_public: public,
            };
            ModuvexError::Domain(Box::new(wrapper))
        }
        ModuvexError::Config(e) => {
            // Config: preserve as Config, annotate message.
            use crate::error::classify::ConfigError;
            let msg = format!("{context_msg}: {e}");
            ModuvexError::Config(ConfigError::new(msg))
        }
        ModuvexError::Lifecycle(e) => {
            // Lifecycle: preserve as Lifecycle, annotate message.
            use crate::error::classify::LifecycleError;
            let module = e.module;
            let msg = format!("{context_msg}: {}", e.message);
            let mut new_err = LifecycleError::new(msg);
            if let Some(m) = module {
                new_err = new_err.in_module(m);
            }
            ModuvexError::Lifecycle(new_err)
        }
        ModuvexError::Other(_) => {
            // Other: wrap in a plain ContextError (no classification to lose).
            let boxed: Box<dyn std::error::Error + Send + Sync> = Box::new(ContextError {
                context: context_msg,
                source: Box::new(original),
            });
            ModuvexError::Other(boxed)
        }
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
        self.map_err(|e| wrap_with_context(e, msg.to_string()))
    }

    fn with_context<F: FnOnce() -> String>(self, f: F) -> Result<T, ModuvexError> {
        self.map_err(|e| wrap_with_context(e, f()))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{classify::ConfigError, ModuvexError};

    fn failing_config() -> Result<(), ModuvexError> {
        Err(ModuvexError::Config(ConfigError::new("bad value")))
    }

    #[test]
    fn static_context_wraps_error() {
        let result = failing_config().context("while loading config");
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("while loading config"), "got: {msg}");
    }

    #[test]
    fn dynamic_context_wraps_error() {
        let name = "UserModule";
        let result = failing_config().with_context(|| format!("while starting {name}"));
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("UserModule"), "got: {msg}");
    }

    // ── Classification preservation tests ─────────────────────────────────────

    #[test]
    fn context_preserves_infra_variant() {
        #[derive(Debug)]
        struct NetError;
        impl fmt::Display for NetError {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "connection refused")
            }
        }
        impl std::error::Error for NetError {}
        impl InfraError for NetError {
            fn is_retryable(&self) -> bool {
                true
            }
            fn retry_after_ms(&self) -> Option<u64> {
                Some(500)
            }
        }

        let result: Result<(), ModuvexError> =
            Err(ModuvexError::Infra(Box::new(NetError))).context("while connecting to DB");

        let err = result.unwrap_err();
        assert!(
            matches!(err, ModuvexError::Infra(_)),
            "should still be Infra, got: {:?}",
            err
        );
        let msg = err.to_string();
        assert!(msg.contains("while connecting to DB"), "got: {msg}");
        assert!(msg.contains("connection refused"), "got: {msg}");
    }

    #[test]
    fn context_preserves_domain_variant() {
        #[derive(Debug)]
        struct NotFound;
        impl fmt::Display for NotFound {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "user not found")
            }
        }
        impl std::error::Error for NotFound {}
        impl DomainError for NotFound {
            fn error_code(&self) -> &str {
                "user.not_found"
            }
            fn http_status(&self) -> u16 {
                404
            }
        }

        let result: Result<(), ModuvexError> =
            Err(ModuvexError::Domain(Box::new(NotFound))).context("while fetching profile");

        let err = result.unwrap_err();
        assert!(
            matches!(err, ModuvexError::Domain(_)),
            "should still be Domain, got: {:?}",
            err
        );
        let msg = err.to_string();
        assert!(msg.contains("while fetching profile"), "got: {msg}");
    }

    #[test]
    fn context_preserves_lifecycle_variant() {
        use crate::error::classify::LifecycleError;

        let result: Result<(), ModuvexError> =
            Err(ModuvexError::Lifecycle(LifecycleError::new("crashed").in_module("AuthModule")))
                .context("during startup");

        let err = result.unwrap_err();
        assert!(
            matches!(err, ModuvexError::Lifecycle(_)),
            "should still be Lifecycle, got: {:?}",
            err
        );
        let msg = err.to_string();
        assert!(msg.contains("during startup"), "got: {msg}");
    }

    #[test]
    fn context_preserves_config_variant() {
        let result: Result<(), ModuvexError> =
            Err(ModuvexError::Config(ConfigError::new("missing").with_key("db.url")))
                .context("reading config");

        let err = result.unwrap_err();
        assert!(
            matches!(err, ModuvexError::Config(_)),
            "should still be Config, got: {:?}",
            err
        );
    }

    #[test]
    fn infra_context_preserves_retryability() {
        #[derive(Debug)]
        struct RetryableErr;
        impl fmt::Display for RetryableErr {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "timeout")
            }
        }
        impl std::error::Error for RetryableErr {}
        impl InfraError for RetryableErr {
            fn is_retryable(&self) -> bool {
                true
            }
            fn retry_after_ms(&self) -> Option<u64> {
                Some(1000)
            }
        }

        let wrapped =
            wrap_with_context(ModuvexError::Infra(Box::new(RetryableErr)), "ctx".to_string());

        if let ModuvexError::Infra(e) = &wrapped {
            assert!(e.is_retryable());
            assert_eq!(e.retry_after_ms(), Some(1000));
        } else {
            panic!("expected Infra variant");
        }
    }

    #[test]
    fn context_error_display_format() {
        let inner: Box<dyn std::error::Error + Send + Sync> =
            Box::new(std::io::Error::new(std::io::ErrorKind::Other, "base error"));
        let ctx_err = ContextError {
            context: "while doing X".to_string(),
            source: inner,
        };
        let s = ctx_err.to_string();
        assert!(s.contains("while doing X"), "got: {s}");
        assert!(s.contains("base error"), "got: {s}");
    }

    #[test]
    fn context_error_source_chain() {
        use std::error::Error;
        let inner: Box<dyn std::error::Error + Send + Sync> =
            Box::new(std::io::Error::new(std::io::ErrorKind::Other, "cause"));
        let ctx_err = ContextError {
            context: "context".to_string(),
            source: inner,
        };
        assert!(ctx_err.source().is_some());
    }

    #[test]
    fn wrap_other_variant_stays_other() {
        #[derive(Debug)]
        struct Misc;
        impl fmt::Display for Misc {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "misc")
            }
        }
        impl std::error::Error for Misc {}

        let err = ModuvexError::Other(Box::new(Misc));
        let wrapped = wrap_with_context(err, "extra context".to_string());
        assert!(
            matches!(wrapped, ModuvexError::Other(_)),
            "Other variant should stay Other"
        );
        assert!(wrapped.to_string().contains("extra context"));
    }

    #[test]
    fn with_context_lazy_closure_not_called_on_ok() {
        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let c = std::sync::Arc::clone(&called);
        let result: Result<u32, ModuvexError> = Ok(42);
        let _ = result.with_context(move || {
            c.store(true, std::sync::atomic::Ordering::SeqCst);
            "should not be called".to_string()
        });
        assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn context_on_ok_passes_through() {
        let result: Result<u32, ModuvexError> = Ok(99);
        let out = result.context("irrelevant context").unwrap();
        assert_eq!(out, 99);
    }

    #[test]
    fn lifecycle_context_contains_both_messages() {
        use crate::error::classify::LifecycleError;
        let result: Result<(), ModuvexError> =
            Err(ModuvexError::Lifecycle(LifecycleError::new("root cause").in_module("M")))
                .context("outer context");
        let err = result.unwrap_err();
        let s = err.to_string();
        assert!(s.contains("outer context"), "got: {s}");
        assert!(s.contains("root cause"), "got: {s}");
    }

    #[test]
    fn config_context_contains_both_messages() {
        let result: Result<(), ModuvexError> =
            Err(ModuvexError::Config(ConfigError::new("original")))
                .context("adding context");
        let err = result.unwrap_err();
        let s = err.to_string();
        assert!(s.contains("adding context"), "got: {s}");
        assert!(s.contains("original"), "got: {s}");
    }

    #[test]
    fn domain_context_preserves_http_status() {
        #[derive(Debug)]
        struct Conflict;
        impl fmt::Display for Conflict {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "conflict")
            }
        }
        impl std::error::Error for Conflict {}
        impl DomainError for Conflict {
            fn error_code(&self) -> &str { "conflict.resource" }
            fn http_status(&self) -> u16 { 409 }
        }

        let wrapped = wrap_with_context(
            ModuvexError::Domain(Box::new(Conflict)),
            "wrapping".to_string(),
        );

        if let ModuvexError::Domain(ref e) = wrapped {
            assert_eq!(e.http_status(), 409);
            assert_eq!(e.error_code(), "conflict.resource");
        } else {
            panic!("expected Domain variant");
        }
    }

    #[test]
    fn non_retryable_infra_context_preserves_non_retryable() {
        #[derive(Debug)]
        struct PermErr;
        impl fmt::Display for PermErr {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "permission denied")
            }
        }
        impl std::error::Error for PermErr {}
        impl InfraError for PermErr {
            fn is_retryable(&self) -> bool { false }
        }

        let wrapped = wrap_with_context(
            ModuvexError::Infra(Box::new(PermErr)),
            "ctx".to_string(),
        );

        if let ModuvexError::Infra(ref e) = wrapped {
            assert!(!e.is_retryable());
        } else {
            panic!("expected Infra variant");
        }
    }
}

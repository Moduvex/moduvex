//! Error system — `ModuvexError` enum with Domain/Infra/Config/Lifecycle classification.

pub mod chain;
pub mod classify;

pub use chain::Context;
pub use classify::{ConfigError, DomainError, InfraError, LifecycleError};

use std::fmt;

// ── ModuvexError ─────────────────────────────────────────────────────────────

/// Top-level framework error.
///
/// All fallible framework operations return `Result<T, ModuvexError>`.
/// The variants classify errors so middleware and error handlers can make
/// informed decisions (retry, surface to user, log silently, etc.).
#[derive(Debug)]
pub enum ModuvexError {
    /// A business-logic violation (validation failure, rule broken, not-found).
    Domain(Box<dyn DomainError>),
    /// An infrastructure failure (DB down, network error, I/O error).
    Infra(Box<dyn InfraError>),
    /// A configuration error detected during Config or Validate phase.
    Config(ConfigError),
    /// A lifecycle-phase error (module failed to start/stop, bad transition).
    Lifecycle(LifecycleError),
    /// Catch-all for errors that don't fit the above categories.
    Other(Box<dyn std::error::Error + Send + Sync + 'static>),
}

impl fmt::Display for ModuvexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Domain(e) => write!(f, "domain error [{}]: {}", e.error_code(), e),
            Self::Infra(e) => write!(f, "infra error (retryable={}): {}", e.is_retryable(), e),
            Self::Config(e) => fmt::Display::fmt(e, f),
            Self::Lifecycle(e) => fmt::Display::fmt(e, f),
            Self::Other(e) => write!(f, "error: {}", e),
        }
    }
}

impl std::error::Error for ModuvexError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Domain(e) => Some(e.as_ref()),
            Self::Infra(e) => Some(e.as_ref()),
            Self::Config(e) => Some(e),
            Self::Lifecycle(e) => Some(e),
            Self::Other(e) => Some(e.as_ref()),
        }
    }
}

// Note: we intentionally do NOT implement
//   `From<ModuvexError> for Box<dyn Error + Send + Sync>`
// because the standard library already provides a blanket
//   `impl<E: Error + Send + Sync> From<E> for Box<dyn Error + Send + Sync>`
// and `ModuvexError` implements `std::error::Error`, so that blanket covers us.
// Adding our own impl would conflict (E0119).

impl From<std::io::Error> for ModuvexError {
    fn from(e: std::io::Error) -> Self {
        // Wrap I/O errors as a retryable infra error.
        #[derive(Debug)]
        struct IoInfraError(std::io::Error);
        impl fmt::Display for IoInfraError {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "I/O error: {}", self.0)
            }
        }
        impl std::error::Error for IoInfraError {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }
        impl InfraError for IoInfraError {
            fn is_retryable(&self) -> bool {
                use std::io::ErrorKind::*;
                matches!(
                    self.0.kind(),
                    ConnectionReset | ConnectionAborted | TimedOut | WouldBlock
                )
            }
        }
        Self::Infra(Box::new(IoInfraError(e)))
    }
}

/// Convenience type alias used throughout the crate.
pub type Result<T> = std::result::Result<T, ModuvexError>;

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Boom;
    impl fmt::Display for Boom {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "boom")
        }
    }
    impl std::error::Error for Boom {}
    impl DomainError for Boom {
        fn error_code(&self) -> &str {
            "test.boom"
        }
        fn http_status(&self) -> u16 {
            500
        }
    }

    #[test]
    fn domain_variant_display() {
        let e = ModuvexError::Domain(Box::new(Boom));
        let s = e.to_string();
        assert!(s.contains("test.boom"), "got: {s}");
        assert!(s.contains("boom"), "got: {s}");
    }

    #[test]
    fn config_variant_display() {
        let e = ModuvexError::Config(ConfigError::new("missing field").with_key("db.url"));
        let s = e.to_string();
        assert!(s.contains("db.url"), "got: {s}");
    }

    #[test]
    fn io_error_converts_to_infra() {
        let io = std::io::Error::new(std::io::ErrorKind::TimedOut, "timed out");
        let e = ModuvexError::from(io);
        assert!(matches!(e, ModuvexError::Infra(_)));
    }

    #[test]
    fn lifecycle_variant_with_module() {
        let e = ModuvexError::Lifecycle(LifecycleError::new("failed").in_module("AuthModule"));
        let s = e.to_string();
        assert!(s.contains("AuthModule"), "got: {s}");
    }

    #[test]
    fn infra_error_display_contains_retryable_flag() {
        #[derive(Debug)]
        struct NetErr;
        impl fmt::Display for NetErr {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "network fail")
            }
        }
        impl std::error::Error for NetErr {}
        impl crate::error::classify::InfraError for NetErr {
            fn is_retryable(&self) -> bool { true }
        }

        let e = ModuvexError::Infra(Box::new(NetErr));
        let s = e.to_string();
        assert!(s.contains("network fail"), "got: {s}");
        assert!(s.contains("retryable=true"), "got: {s}");
    }

    #[test]
    fn other_error_variant_display() {
        #[derive(Debug)]
        struct Plain;
        impl fmt::Display for Plain {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "plain error")
            }
        }
        impl std::error::Error for Plain {}

        let e = ModuvexError::Other(Box::new(Plain));
        let s = e.to_string();
        assert!(s.contains("plain error"), "got: {s}");
    }

    #[test]
    fn moduvex_error_source_chain_present() {
        let e = ModuvexError::Config(ConfigError::new("inner error").with_key("k"));
        let err: &dyn std::error::Error = &e;
        // source() is Some(ConfigError)
        assert!(err.source().is_some());
    }

    #[test]
    fn io_error_not_retryable_for_permission_denied() {
        let io = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let e = ModuvexError::from(io);
        if let ModuvexError::Infra(ref infra) = e {
            assert!(!infra.is_retryable());
        } else {
            panic!("expected Infra variant");
        }
    }

    #[test]
    fn io_error_retryable_for_connection_reset() {
        let io = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "reset");
        let e = ModuvexError::from(io);
        if let ModuvexError::Infra(ref infra) = e {
            assert!(infra.is_retryable());
        } else {
            panic!("expected Infra variant");
        }
    }

    #[test]
    fn io_error_retryable_for_would_block() {
        let io = std::io::Error::new(std::io::ErrorKind::WouldBlock, "block");
        let e = ModuvexError::from(io);
        if let ModuvexError::Infra(ref infra) = e {
            assert!(infra.is_retryable());
        } else {
            panic!("expected Infra variant");
        }
    }

    #[test]
    fn config_variant_source_is_config_error() {
        let e = ModuvexError::Config(ConfigError::new("cfg").with_key("x"));
        use std::error::Error;
        let src = e.source().unwrap();
        // The source should display the config error
        assert!(src.to_string().contains("cfg"));
    }
}

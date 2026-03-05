//! Integration tests for `#[derive(InfraError)]`.
//!
//! Verifies Display, std::error::Error, InfraError::is_retryable,
//! default retryable=false behaviour, and From<Self> for ModuvexError.

use moduvex_core::{InfraError, ModuvexError};

// ---------------------------------------------------------------------------
// Basic retryable + non-retryable variants
// ---------------------------------------------------------------------------

#[derive(moduvex_macros::InfraError, Debug)]
enum DbError {
    #[error(retryable = true)]
    ConnectionLost(String),

    #[error(retryable = false)]
    InvalidQuery(String),
}

#[test]
fn retryable_variant_is_retryable() {
    assert!(DbError::ConnectionLost("timeout".into()).is_retryable());
}

#[test]
fn non_retryable_variant_is_not_retryable() {
    assert!(!DbError::InvalidQuery("bad sql".into()).is_retryable());
}

#[test]
fn infra_error_display_shows_variant_name() {
    assert_eq!(
        format!("{}", DbError::ConnectionLost("x".into())),
        "ConnectionLost"
    );
    assert_eq!(
        format!("{}", DbError::InvalidQuery("y".into())),
        "InvalidQuery"
    );
}

#[test]
fn infra_error_is_std_error() {
    fn _assert_error<E: std::error::Error>() {}
    _assert_error::<DbError>();
}

// ---------------------------------------------------------------------------
// Default retryable when attribute omitted
// ---------------------------------------------------------------------------

#[derive(moduvex_macros::InfraError, Debug)]
enum NetworkError {
    #[error(retryable = false)]
    DnsFailure,

    // No retryable attr — parser defaults to false
    #[error(retryable = false)]
    Timeout,
}

#[test]
fn default_retryable_is_false() {
    assert!(!NetworkError::DnsFailure.is_retryable());
    assert!(!NetworkError::Timeout.is_retryable());
}

// ---------------------------------------------------------------------------
// Named-field variants
// ---------------------------------------------------------------------------

#[derive(moduvex_macros::InfraError, Debug)]
enum StorageError {
    #[error(retryable = true)]
    WriteFailure { path: String, reason: String },

    #[error(retryable = false)]
    PermissionDenied { path: String },
}

#[test]
fn named_field_variant_retryable() {
    let e = StorageError::WriteFailure {
        path: "/tmp/x".into(),
        reason: "disk full".into(),
    };
    assert!(e.is_retryable());
}

#[test]
fn named_field_variant_not_retryable() {
    let e = StorageError::PermissionDenied { path: "/etc/passwd".into() };
    assert!(!e.is_retryable());
}

#[test]
fn named_field_variant_display_shows_variant_name() {
    let e = StorageError::PermissionDenied { path: "/etc".into() };
    assert_eq!(format!("{e}"), "PermissionDenied");
}

// ---------------------------------------------------------------------------
// Unit variants
// ---------------------------------------------------------------------------

#[derive(moduvex_macros::InfraError, Debug)]
enum QueueError {
    #[error(retryable = true)]
    Full,

    #[error(retryable = false)]
    Poisoned,
}

#[test]
fn unit_variant_retryable_true() {
    assert!(QueueError::Full.is_retryable());
}

#[test]
fn unit_variant_retryable_false() {
    assert!(!QueueError::Poisoned.is_retryable());
}

// ---------------------------------------------------------------------------
// From<InfraError> for ModuvexError
// ---------------------------------------------------------------------------

#[test]
fn infra_error_converts_into_moduvex_error() {
    let e: ModuvexError = DbError::ConnectionLost("lost".into()).into();
    assert!(matches!(e, ModuvexError::Infra(_)));
}

#[test]
fn infra_error_moduvex_display_contains_retryable_flag() {
    let e: ModuvexError = DbError::ConnectionLost("lost".into()).into();
    let s = e.to_string();
    // ModuvexError::Infra formats as "infra error (retryable=true): ConnectionLost"
    assert!(s.contains("retryable=true"), "got: {s}");
    assert!(s.contains("ConnectionLost"), "got: {s}");
}

#[test]
fn non_retryable_infra_moduvex_display() {
    let e: ModuvexError = DbError::InvalidQuery("bad".into()).into();
    let s = e.to_string();
    assert!(s.contains("retryable=false"), "got: {s}");
}

// ---------------------------------------------------------------------------
// Send + Sync bounds
// ---------------------------------------------------------------------------

#[test]
fn infra_error_impls_are_send_sync() {
    fn _assert<T: Send + Sync>() {}
    _assert::<DbError>();
    _assert::<StorageError>();
    _assert::<QueueError>();
}

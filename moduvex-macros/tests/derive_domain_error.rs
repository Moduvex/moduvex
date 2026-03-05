//! Integration tests for `#[derive(DomainError)]`.
//!
//! Verifies Display, std::error::Error, DomainError trait methods,
//! and From<Self> for ModuvexError conversions.

use moduvex_core::{DomainError, ModuvexError};

// ---------------------------------------------------------------------------
// Basic single-variant enum
// ---------------------------------------------------------------------------

#[derive(moduvex_macros::DomainError, Debug)]
enum NotFoundError {
    #[error(code = "RESOURCE_NOT_FOUND", status = 404)]
    Resource,
}

#[test]
fn domain_error_code_matches_attribute() {
    assert_eq!(NotFoundError::Resource.error_code(), "RESOURCE_NOT_FOUND");
}

#[test]
fn domain_error_http_status_matches_attribute() {
    assert_eq!(NotFoundError::Resource.http_status(), 404);
}

#[test]
fn domain_error_display_shows_code() {
    let s = format!("{}", NotFoundError::Resource);
    assert_eq!(s, "RESOURCE_NOT_FOUND");
}

#[test]
fn domain_error_is_std_error() {
    fn _assert_error<E: std::error::Error>() {}
    _assert_error::<NotFoundError>();
}

#[test]
fn domain_error_is_public_by_default() {
    assert!(NotFoundError::Resource.is_public());
}

// ---------------------------------------------------------------------------
// Multi-variant enum — unnamed tuple fields
// ---------------------------------------------------------------------------

#[derive(moduvex_macros::DomainError, Debug)]
enum UserError {
    #[error(code = "USER_NOT_FOUND", status = 404)]
    NotFound(u64),

    #[error(code = "EMAIL_ALREADY_EXISTS", status = 409)]
    EmailExists(String),

    #[error(code = "INVALID_INPUT", status = 422)]
    InvalidInput(String, String), // two fields
}

#[test]
fn multi_variant_not_found_code() {
    assert_eq!(UserError::NotFound(42).error_code(), "USER_NOT_FOUND");
}

#[test]
fn multi_variant_not_found_status() {
    assert_eq!(UserError::NotFound(42).http_status(), 404);
}

#[test]
fn multi_variant_email_exists_code() {
    assert_eq!(
        UserError::EmailExists("a@b.com".into()).error_code(),
        "EMAIL_ALREADY_EXISTS"
    );
}

#[test]
fn multi_variant_email_exists_status() {
    assert_eq!(UserError::EmailExists("a@b.com".into()).http_status(), 409);
}

#[test]
fn multi_variant_invalid_input_display() {
    let s = format!("{}", UserError::InvalidInput("f".into(), "msg".into()));
    assert_eq!(s, "INVALID_INPUT");
}

// ---------------------------------------------------------------------------
// Variant with named fields
// ---------------------------------------------------------------------------

#[derive(moduvex_macros::DomainError, Debug)]
enum OrderError {
    #[error(code = "ORDER_NOT_FOUND", status = 404)]
    NotFound { order_id: u64 },

    #[error(code = "ORDER_CANCELLED", status = 410)]
    Cancelled { reason: String },
}

#[test]
fn named_field_variant_code() {
    let e = OrderError::NotFound { order_id: 1 };
    assert_eq!(e.error_code(), "ORDER_NOT_FOUND");
}

#[test]
fn named_field_variant_status() {
    let e = OrderError::Cancelled { reason: "user requested".into() };
    assert_eq!(e.http_status(), 410);
}

// ---------------------------------------------------------------------------
// Unit variants (no fields)
// ---------------------------------------------------------------------------

#[derive(moduvex_macros::DomainError, Debug)]
enum AuthError {
    #[error(code = "UNAUTHORIZED", status = 401)]
    Unauthorized,

    #[error(code = "FORBIDDEN", status = 403)]
    Forbidden,
}

#[test]
fn unit_variant_unauthorized_code_and_status() {
    assert_eq!(AuthError::Unauthorized.error_code(), "UNAUTHORIZED");
    assert_eq!(AuthError::Unauthorized.http_status(), 401);
}

#[test]
fn unit_variant_forbidden_code_and_status() {
    assert_eq!(AuthError::Forbidden.error_code(), "FORBIDDEN");
    assert_eq!(AuthError::Forbidden.http_status(), 403);
}

// ---------------------------------------------------------------------------
// From<DomainError> for ModuvexError
// ---------------------------------------------------------------------------

#[test]
fn domain_error_converts_into_moduvex_error() {
    let e: ModuvexError = AuthError::Unauthorized.into();
    assert!(matches!(e, ModuvexError::Domain(_)));
}

#[test]
fn domain_error_moduvex_display_contains_code() {
    let e: ModuvexError = AuthError::Forbidden.into();
    let s = e.to_string();
    assert!(s.contains("FORBIDDEN"), "got: {s}");
}

// ---------------------------------------------------------------------------
// Boundary HTTP status values (100 and 599)
// ---------------------------------------------------------------------------

#[derive(moduvex_macros::DomainError, Debug)]
enum BoundaryError {
    #[error(code = "INFO", status = 100)]
    Informational,

    #[error(code = "SERVER_ERR", status = 599)]
    ServerMax,
}

#[test]
fn boundary_status_100() {
    assert_eq!(BoundaryError::Informational.http_status(), 100);
}

#[test]
fn boundary_status_599() {
    assert_eq!(BoundaryError::ServerMax.http_status(), 599);
}

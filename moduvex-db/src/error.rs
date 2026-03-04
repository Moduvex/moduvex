//! `DbError` — typed error enum for moduvex-db.
//!
//! All public API functions return `Result<T, DbError>`.

use std::fmt;
use std::io;

// ── DbError ───────────────────────────────────────────────────────────────────

/// Errors produced by moduvex-db operations.
#[derive(Debug)]
pub enum DbError {
    /// Underlying I/O failure (socket read/write, OS error).
    Io(io::Error),
    /// PostgreSQL server returned an error response.
    ServerError {
        /// SQLSTATE error code (e.g. "23505" for unique violation).
        code: String,
        /// Human-readable message from the server.
        message: String,
        /// Optional detail field from the server.
        detail: Option<String>,
    },
    /// Authentication with the server failed.
    AuthFailed(String),
    /// The requested authentication method is not supported.
    UnsupportedAuth(String),
    /// Wire protocol violation — unexpected or malformed message.
    Protocol(String),
    /// Pool checkout timed out (no connections available within the timeout).
    PoolTimeout,
    /// Pool is shut down; no new connections can be checked out.
    PoolClosed,
    /// Type mapping error — cannot convert between Rust and PostgreSQL types.
    TypeMismatch(String),
    /// Migration error — failed to apply or track a migration.
    Migration(String),
    /// Attempted to use a transaction that has already been consumed.
    TransactionConsumed,
    /// A null value was encountered where a non-null value was required.
    NullValue { column: String },
    /// Generic string error for one-off cases.
    Other(String),
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DbError::Io(e)            => write!(f, "I/O error: {e}"),
            DbError::ServerError { code, message, detail } => {
                write!(f, "PostgreSQL error [{code}]: {message}")?;
                if let Some(d) = detail { write!(f, " — {d}")?; }
                Ok(())
            }
            DbError::AuthFailed(msg)        => write!(f, "authentication failed: {msg}"),
            DbError::UnsupportedAuth(m)     => write!(f, "unsupported auth method: {m}"),
            DbError::Protocol(msg)          => write!(f, "protocol error: {msg}"),
            DbError::PoolTimeout            => write!(f, "connection pool timeout"),
            DbError::PoolClosed             => write!(f, "connection pool is closed"),
            DbError::TypeMismatch(msg)      => write!(f, "type mismatch: {msg}"),
            DbError::Migration(msg)         => write!(f, "migration error: {msg}"),
            DbError::TransactionConsumed    => write!(f, "transaction already committed or rolled back"),
            DbError::NullValue { column }   => write!(f, "null value in column '{column}'"),
            DbError::Other(msg)             => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for DbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DbError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for DbError {
    fn from(e: io::Error) -> Self { DbError::Io(e) }
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, DbError>;

// ── OtherError ────────────────────────────────────────────────────────────────

/// Simple string error wrapper for use with `moduvex_core::ModuvexError::Other`.
#[derive(Debug)]
pub struct OtherError(pub String);

impl fmt::Display for OtherError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.0) }
}

impl std::error::Error for OtherError {}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_server_error() {
        let e = DbError::ServerError {
            code: "23505".into(),
            message: "duplicate key".into(),
            detail: Some("Key (id)=(1) already exists.".into()),
        };
        let s = e.to_string();
        assert!(s.contains("23505"));
        assert!(s.contains("duplicate key"));
        assert!(s.contains("already exists"));
    }

    #[test]
    fn display_io_error() {
        let e = DbError::Io(io::Error::new(io::ErrorKind::ConnectionRefused, "refused"));
        assert!(e.to_string().contains("I/O error"));
    }

    #[test]
    fn from_io_error() {
        let io_err = io::Error::new(io::ErrorKind::BrokenPipe, "broken");
        let db_err: DbError = io_err.into();
        assert!(matches!(db_err, DbError::Io(_)));
    }

    #[test]
    fn null_value_display() {
        let e = DbError::NullValue { column: "email".into() };
        assert!(e.to_string().contains("email"));
    }
}

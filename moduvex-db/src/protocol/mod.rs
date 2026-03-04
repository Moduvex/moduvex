//! Protocol abstraction layer.
//!
//! Currently only PostgreSQL is implemented. The `DatabaseProtocol` trait
//! defines the interface for future drivers (MySQL, SQLite, etc.).

pub mod postgres;

use std::future::Future;
use std::pin::Pin;

use crate::error::Result;
use crate::protocol::postgres::PgRowSet;

// ── DatabaseProtocol ──────────────────────────────────────────────────────────

/// Minimal async trait for a database connection protocol.
///
/// Object-safe via boxed futures. Implementors: `PgConnection`.
pub trait DatabaseProtocol: Send + 'static {
    /// Execute a query and return a raw result set.
    fn query<'a>(
        &'a mut self,
        sql: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<PgRowSet>> + Send + 'a>>;

    /// Execute a statement and return rows affected.
    fn execute<'a>(
        &'a mut self,
        sql: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<u64>> + Send + 'a>>;

    /// Ping the server to verify liveness.
    fn ping<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
}

impl DatabaseProtocol for postgres::PgConnection {
    fn query<'a>(
        &'a mut self,
        sql: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<PgRowSet>> + Send + 'a>> {
        Box::pin(self.query(sql))
    }

    fn execute<'a>(
        &'a mut self,
        sql: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<u64>> + Send + 'a>> {
        Box::pin(self.execute(sql))
    }

    fn ping<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(self.ping())
    }
}

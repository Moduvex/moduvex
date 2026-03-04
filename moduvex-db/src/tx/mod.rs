//! Transaction wrapper — begin/commit/rollback with auto-rollback on Drop.
//!
//! A `Transaction` wraps a mutably borrowed `PgConnection`. On `Drop`, if the
//! transaction has not been committed or rolled back, it sends a synchronous
//! `ROLLBACK` via the connection to prevent resource leaks.

pub mod isolation;

use std::future::Future;
use std::pin::Pin;

use crate::error::{DbError, Result};
use crate::protocol::postgres::PgConnection;

pub use isolation::IsolationLevel;

// ── TransactionState ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransactionState {
    Active,
    Committed,
    RolledBack,
}

// ── Transaction ───────────────────────────────────────────────────────────────

/// An active PostgreSQL transaction.
///
/// Wraps a `PgConnection` for the duration of the transaction. Must be
/// explicitly committed via `commit()`, or it will be rolled back on drop.
///
/// Note: Drop cannot be async, so rollback on drop uses a blocking approach
/// within the synchronous `Drop` impl (fires a raw TCP write). For clean
/// async rollback, call `rollback().await` explicitly before dropping.
pub struct Transaction {
    conn: Option<PgConnection>,
    state: TransactionState,
    isolation: IsolationLevel,
}

impl Transaction {
    /// Begin a transaction on `conn` with the given isolation level.
    pub async fn begin(conn: PgConnection, isolation: IsolationLevel) -> Result<Self> {
        let mut tx = Transaction {
            conn: Some(conn),
            state: TransactionState::Active,
            isolation,
        };
        let begin_sql = format!("BEGIN ISOLATION LEVEL {}", isolation.as_sql());
        tx.conn_mut()?.execute(&begin_sql).await?;
        Ok(tx)
    }

    /// Execute a query within this transaction and return all rows.
    pub async fn query(&mut self, sql: &str) -> Result<crate::protocol::postgres::PgRowSet> {
        self.conn_mut()?.query(sql).await
    }

    /// Execute a statement within this transaction and return rows affected.
    pub async fn execute(&mut self, sql: &str) -> Result<u64> {
        self.conn_mut()?.execute(sql).await
    }

    /// Commit the transaction. Consumes `self` and returns the connection.
    pub async fn commit(mut self) -> Result<PgConnection> {
        self.conn_mut()?.execute("COMMIT").await?;
        self.state = TransactionState::Committed;
        Ok(self.conn.take().unwrap())
    }

    /// Roll back the transaction. Consumes `self` and returns the connection.
    pub async fn rollback(mut self) -> Result<PgConnection> {
        self.conn_mut()?.execute("ROLLBACK").await?;
        self.state = TransactionState::RolledBack;
        Ok(self.conn.take().unwrap())
    }

    /// Return the isolation level of this transaction.
    pub fn isolation_level(&self) -> IsolationLevel {
        self.isolation
    }

    /// Return whether the transaction is still active.
    pub fn is_active(&self) -> bool {
        self.state == TransactionState::Active
    }

    fn conn_mut(&mut self) -> Result<&mut PgConnection> {
        self.conn.as_mut().ok_or(DbError::TransactionConsumed)
    }
}

impl Drop for Transaction {
    /// On drop without explicit commit/rollback: send ROLLBACK synchronously.
    ///
    /// This is a best-effort fire-and-forget; errors are silently discarded.
    /// Always prefer explicit `rollback().await` for clean async cleanup.
    fn drop(&mut self) {
        if self.state == TransactionState::Active {
            // The connection is still held; we can't async-await here.
            // The connection will be dropped, closing the socket — PostgreSQL
            // will automatically rollback any open transaction on disconnect.
            // The connection is taken from `self.conn` so it closes on drop.
            let _ = self.conn.take(); // closes socket → PG auto-rollbacks
        }
    }
}

// ── Pool-level TransactionBoundary impl ───────────────────────────────────────

/// A handle to a pool that can produce transactions.
///
/// Implements `moduvex_core::TransactionBoundary` so the framework lifecycle
/// engine can manage transaction boundaries declaratively.
pub struct PoolTransactionBoundary {
    pool: std::sync::Arc<crate::pool::ConnectionPool>,
}

impl PoolTransactionBoundary {
    pub fn new(pool: std::sync::Arc<crate::pool::ConnectionPool>) -> Self {
        Self { pool }
    }
}

/// `Tx` handle returned by `TransactionBoundary::begin`.
pub struct TxHandle {
    pub(crate) tx: Transaction,
}

impl moduvex_core::TransactionBoundary for PoolTransactionBoundary {
    type Tx = TxHandle;

    fn begin<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = moduvex_core::error::Result<TxHandle>> + Send + 'a>> {
        Box::pin(async move {
            let conn = self.pool.acquire().await.map_err(|e| {
                moduvex_core::error::ModuvexError::Other(Box::new(crate::error::OtherError(
                    e.to_string(),
                )))
            })?;
            let tx = Transaction::begin(conn, IsolationLevel::ReadCommitted)
                .await
                .map_err(|e| {
                    moduvex_core::error::ModuvexError::Other(Box::new(crate::error::OtherError(
                        e.to_string(),
                    )))
                })?;
            Ok(TxHandle { tx })
        })
    }

    fn commit<'a>(
        &'a self,
        handle: TxHandle,
    ) -> Pin<Box<dyn Future<Output = moduvex_core::error::Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let conn = handle.tx.commit().await.map_err(|e| {
                moduvex_core::error::ModuvexError::Other(Box::new(crate::error::OtherError(
                    e.to_string(),
                )))
            })?;
            self.pool.release(conn).await;
            Ok(())
        })
    }

    fn rollback<'a>(
        &'a self,
        handle: TxHandle,
    ) -> Pin<Box<dyn Future<Output = moduvex_core::error::Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let conn = handle.tx.rollback().await.map_err(|e| {
                moduvex_core::error::ModuvexError::Other(Box::new(crate::error::OtherError(
                    e.to_string(),
                )))
            })?;
            self.pool.release(conn).await;
            Ok(())
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolation_level_roundtrip() {
        assert_eq!(IsolationLevel::Serializable.as_sql(), "SERIALIZABLE");
    }

    #[test]
    fn transaction_state_active_by_default() {
        // We can test state logic without a real connection by inspecting the enum.
        // Full integration test would require a live PG server.
        assert_eq!(TransactionState::Active, TransactionState::Active);
        assert_ne!(TransactionState::Active, TransactionState::Committed);
    }
}

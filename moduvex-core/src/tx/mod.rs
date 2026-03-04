//! Transaction boundary abstractions — stub for Phase 5 (moduvex-db).
//!
//! The `TransactionBoundary` trait defines the unit-of-work contract.
//! Concrete implementations (Postgres, SQLite, etc.) live in `moduvex-db`.
//! This stub keeps `moduvex-core` self-contained and allows higher-level
//! crates to depend on the trait without pulling in a database driver.

use std::future::Future;
use std::pin::Pin;

use crate::error::Result;

// ── TransactionBoundary ───────────────────────────────────────────────────────

/// A unit-of-work boundary — begin, commit, or roll back a transaction.
///
/// Implementors are typically thin wrappers around a database connection pool.
/// The trait is object-safe via boxed futures.
pub trait TransactionBoundary: Send + Sync + 'static {
    /// The transaction handle type produced by `begin`.
    type Tx: Send + 'static;

    /// Begin a new transaction, returning a handle.
    fn begin<'a>(&'a self)
        -> Pin<Box<dyn Future<Output = Result<Self::Tx>> + Send + 'a>>;

    /// Commit the transaction.
    fn commit<'a>(&'a self, tx: Self::Tx)
        -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

    /// Roll back the transaction.
    fn rollback<'a>(&'a self, tx: Self::Tx)
        -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// A no-op in-memory transaction boundary used to verify the trait compiles.
    struct NoopTx;

    struct NoopBoundary;

    impl TransactionBoundary for NoopBoundary {
        type Tx = NoopTx;

        fn begin<'a>(&'a self)
            -> Pin<Box<dyn Future<Output = Result<NoopTx>> + Send + 'a>>
        {
            Box::pin(async { Ok(NoopTx) })
        }

        fn commit<'a>(&'a self, _tx: NoopTx)
            -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>
        {
            Box::pin(async { Ok(()) })
        }

        fn rollback<'a>(&'a self, _tx: NoopTx)
            -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>
        {
            Box::pin(async { Ok(()) })
        }
    }

    #[test]
    fn noop_boundary_compiles_and_runs() {
        moduvex_runtime::block_on(async {
            let b = NoopBoundary;
            let tx = b.begin().await.unwrap();
            b.commit(tx).await.unwrap();

            let tx2 = b.begin().await.unwrap();
            b.rollback(tx2).await.unwrap();
        });
    }
}

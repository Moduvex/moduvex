//! `moduvex-db` — Database abstraction layer for the Moduvex framework.
//!
//! # MVP Features
//! - PostgreSQL wire protocol (simple query, MD5 auth, text-format types)
//! - Async connection pool (LIFO idle list, semaphore-bounded, health monitor)
//! - Parameterized queries with SQL-injection-safe parameter binding
//! - Fluent `QueryBuilder` for SELECT queries
//! - Transactions with begin/commit/rollback and auto-rollback on Drop
//! - File-based migration engine (up-only, version-tracked)
//!
//! # Quick Start
//! ```rust,no_run
//! use moduvex_db::{ConnectionPool, PoolConfig, QueryBuilder, Order};
//!
//! let cfg = PoolConfig::new("postgres://user:pass@127.0.0.1:5432/mydb");
//! let pool = ConnectionPool::new(cfg);
//!
//! moduvex_runtime::block_on(async {
//!     let mut conn = pool.acquire().await.unwrap();
//!     let sql = QueryBuilder::select("users")
//!         .columns(&["id", "name"])
//!         .where_eq("active", true)
//!         .order_by("id", Order::Asc)
//!         .limit(10)
//!         .build_inlined()
//!         .unwrap();
//!     let rowset = conn.query(&sql).await.unwrap();
//!     pool.release(conn).await;
//! });
//! ```

// ── Crate modules ─────────────────────────────────────────────────────────────

pub mod error;
pub mod migrate;
pub mod pool;
pub mod protocol;
pub mod query;
pub mod tx;

// ── Top-level re-exports ──────────────────────────────────────────────────────

// Error types
pub use error::{DbError, Result};

// Pool
pub use pool::health::{run_health_sweep, spawn_health_monitor, HealthMonitorConfig};
pub use pool::{ConnectionPool, PoolConfig};

// Protocol — raw PG types
pub use protocol::postgres::{PgColumn, PgConnection, PgRow, PgRowSet};

// Query layer — typed accessors
pub use query::builder::QueryBuilder as Query;
pub use query::param::substitute_params;
pub use query::{Column, FromRow, Order, Param, QueryBuilder, Row, RowSet, ToParam};

// Transaction
pub use tx::{IsolationLevel, Transaction};
pub use tx::{PoolTransactionBoundary, TxHandle};

// Migration
pub use migrate::runner::{load_migrations, parse_version, Migration};
pub use migrate::{MigrationEngine, MigrationReport};

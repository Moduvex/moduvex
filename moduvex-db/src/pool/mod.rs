//! `ConnectionPool` — async PostgreSQL connection pool.
//!
//! Design:
//! - Idle connections stored in a `VecDeque<PgConnection>` (LIFO checkout).
//! - Bounded by `max_size`; checkout waits if all connections are checked out.
//! - A `Mutex<PoolInner>` guards the idle list and live count.
//! - Callers `acquire()` a connection; `release()` returns it.
//! - Background health task (see `health.rs`) pings idle conns periodically.

pub mod config;
pub mod health;

pub use config::PoolConfig;

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use moduvex_runtime::sync::Mutex;

use crate::error::{DbError, Result};
use crate::protocol::postgres::PgConnection;

// ── PoolInner ─────────────────────────────────────────────────────────────────

/// Guards the mutable pool state.
pub(crate) struct PoolInner {
    /// Idle connections available for immediate checkout (LIFO).
    idle: VecDeque<IdleConn>,
    /// Total live connections (idle + checked-out).
    live: usize,
    /// True after `close()` is called; no new connections will be created.
    closed: bool,
}

struct IdleConn {
    conn: PgConnection,
    /// When this connection was returned to the pool.
    idle_since: Instant,
}

// ── ConnectionPool ────────────────────────────────────────────────────────────

/// Async PostgreSQL connection pool.
pub struct ConnectionPool {
    cfg: PoolConfig,
    inner: Arc<Mutex<PoolInner>>,
}

impl ConnectionPool {
    /// Create a new pool. Does not open any connections immediately.
    pub fn new(cfg: PoolConfig) -> Arc<Self> {
        cfg.validate().expect("invalid PoolConfig");
        Arc::new(Self {
            inner: Arc::new(Mutex::new(PoolInner {
                idle: VecDeque::new(),
                live: 0,
                closed: false,
            })),
            cfg,
        })
    }

    /// Acquire a connection from the pool.
    ///
    /// - Pops an idle connection (LIFO) if available.
    /// - Creates a new connection if `live < max_size`.
    /// - Returns `DbError::PoolTimeout` if no connection available within `connect_timeout`.
    /// - Returns `DbError::PoolClosed` if the pool has been shut down.
    pub async fn acquire(&self) -> Result<PgConnection> {
        let deadline = Instant::now() + self.cfg.connect_timeout;

        loop {
            // ── Try to get an idle conn or reserve a slot ──────────────────
            let maybe_conn_or_create = {
                let mut g = self.inner.lock().await;
                if g.closed {
                    return Err(DbError::PoolClosed);
                }
                if let Some(idle) = g.idle.pop_back() {
                    // LIFO: pop from the back (most recently used)
                    Some(Ok(idle.conn))
                } else if g.live < self.cfg.max_size {
                    g.live += 1;
                    Some(Err(())) // signal: create a new connection
                } else {
                    None // pool exhausted; will wait below
                }
            };

            match maybe_conn_or_create {
                Some(Ok(conn)) => return Ok(conn),
                Some(Err(())) => {
                    // Slot reserved; open a new connection
                    match self.open_connection().await {
                        Ok(conn) => return Ok(conn),
                        Err(e) => {
                            // Release the reserved slot on failure
                            let mut g = self.inner.lock().await;
                            g.live -= 1;
                            return Err(e);
                        }
                    }
                }
                None => {
                    // Pool exhausted — yield and retry with timeout
                    if Instant::now() >= deadline {
                        return Err(DbError::PoolTimeout);
                    }
                    // Small yield to let other tasks return connections
                    moduvex_runtime::sleep(std::time::Duration::from_millis(5)).await;
                }
            }
        }
    }

    /// Return a connection to the pool.
    ///
    /// If the pool is closed or at capacity, the connection is dropped (closed).
    pub async fn release(&self, conn: PgConnection) {
        let mut g = self.inner.lock().await;
        if g.closed || g.idle.len() >= self.cfg.max_size {
            // Drop the connection outside the lock
            g.live = g.live.saturating_sub(1);
            drop(g);
            // conn drops here, closing the socket
            return;
        }
        g.idle.push_back(IdleConn {
            conn,
            idle_since: Instant::now(),
        });
    }

    /// Close the pool: mark it closed, drain idle connections.
    pub async fn close(&self) {
        let mut g = self.inner.lock().await;
        g.closed = true;
        // Drain idle connections (they'll be dropped → sockets closed)
        let drained: Vec<_> = g.idle.drain(..).collect();
        g.live = 0;
        drop(g);
        drop(drained); // close sockets outside lock
    }

    /// Current number of idle connections in the pool.
    pub async fn idle_count(&self) -> usize {
        self.inner.lock().await.idle.len()
    }

    /// Current total live connections (idle + checked-out).
    pub async fn live_count(&self) -> usize {
        self.inner.lock().await.live
    }

    /// Pool configuration.
    pub fn config(&self) -> &PoolConfig {
        &self.cfg
    }

    /// Internal accessor for health monitor.
    pub(crate) fn inner(&self) -> &Arc<Mutex<PoolInner>> {
        &self.inner
    }

    /// Open a fresh PostgreSQL connection using the pool's config URL.
    async fn open_connection(&self) -> Result<PgConnection> {
        let (user, password, host_port, database) = parse_url(&self.cfg.database_url)?;
        PgConnection::connect(&host_port, &user, &password, &database).await
    }
}

// ── URL parser ────────────────────────────────────────────────────────────────

/// Parse `postgres://user:password@host:port/database` into components.
///
/// Returns `(user, password, "host:port", database)`.
pub(crate) fn parse_url(url: &str) -> Result<(String, String, String, String)> {
    // Strip scheme
    let rest = url
        .strip_prefix("postgres://")
        .or_else(|| url.strip_prefix("postgresql://"))
        .ok_or_else(|| DbError::Other(format!("unsupported URL scheme: {url}")))?;

    // Split user:pass@host:port/db
    let (userinfo, hostpath) = rest
        .split_once('@')
        .ok_or_else(|| DbError::Other(format!("missing '@' in database URL: {url}")))?;

    let (user, password) = userinfo
        .split_once(':')
        .map(|(u, p)| (u.to_string(), p.to_string()))
        .unwrap_or_else(|| (userinfo.to_string(), String::new()));

    let (hostport, database) = hostpath
        .split_once('/')
        .ok_or_else(|| DbError::Other(format!("missing database name in URL: {url}")))?;

    Ok((user, password, hostport.to_string(), database.to_string()))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_url_full() {
        let (user, pass, hostport, db) =
            parse_url("postgres://alice:secret@127.0.0.1:5432/mydb").unwrap();
        assert_eq!(user, "alice");
        assert_eq!(pass, "secret");
        assert_eq!(hostport, "127.0.0.1:5432");
        assert_eq!(db, "mydb");
    }

    #[test]
    fn parse_url_postgresql_scheme() {
        let (user, _, _, db) = parse_url("postgresql://bob:pw@localhost:5432/testdb").unwrap();
        assert_eq!(user, "bob");
        assert_eq!(db, "testdb");
    }

    #[test]
    fn parse_url_invalid_scheme() {
        assert!(parse_url("mysql://user:pw@host/db").is_err());
    }

    #[test]
    fn parse_url_missing_at() {
        assert!(parse_url("postgres://user:pw/db").is_err());
    }

    #[test]
    fn parse_url_missing_db() {
        assert!(parse_url("postgres://user:pw@host:5432").is_err());
    }

    #[test]
    fn pool_config_validate() {
        let cfg = PoolConfig::new("postgres://u:p@localhost:5432/db");
        assert!(cfg.validate().is_ok());
    }
}

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
use std::task::Waker;
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
    /// Wakers for tasks waiting to acquire a connection.
    waiters: Vec<Waker>,
}

struct IdleConn {
    conn: PgConnection,
    /// When this connection was first created.
    created_at: Instant,
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
                waiters: Vec::new(),
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
                    // Pool exhausted — register waker and wait for release() notification
                    if Instant::now() >= deadline {
                        return Err(DbError::PoolTimeout);
                    }
                    // Wait via poll_fn: registers waker in pool, then yields.
                    // release() will wake us; timeout fallback via spawned sleep.
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    wait_for_release(Arc::clone(&self.inner), remaining).await;
                }
            }
        }
    }

    /// Return a connection to the pool.
    ///
    /// If the pool is closed or at capacity, the connection is dropped (closed).
    /// Wakes one waiting `acquire()` caller if any are queued.
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
            created_at: Instant::now(),
            idle_since: Instant::now(),
        });
        // Wake exactly one waiting acquire() caller
        if let Some(waker) = g.waiters.pop() {
            waker.wake();
        }
    }

    /// Close the pool: mark it closed, drain idle connections.
    ///
    /// Does NOT zero `live` — checked-out connections are still alive and will
    /// decrement `live` when returned via `release()`.
    pub async fn close(&self) {
        let mut g = self.inner.lock().await;
        g.closed = true;
        let idle_count = g.idle.len();
        let drained: Vec<_> = g.idle.drain(..).collect();
        // Only subtract the idle connections we actually drained
        g.live = g.live.saturating_sub(idle_count);
        // Wake any tasks blocked in acquire() so they get PoolClosed
        for waker in g.waiters.drain(..) {
            waker.wake();
        }
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

// ── Wait-for-release helper ──────────────────────────────────────────────────

/// Register the current task's waker in the pool's waiter list, then sleep
/// until either `release()` wakes us or the timeout expires.
///
/// This replaces the naive 5ms spin-wait with a targeted notification:
/// `release()` calls `waker.wake()` on exactly one waiter, eliminating
/// thundering-herd and reducing latency from up to 5ms to near-zero.
async fn wait_for_release(inner: Arc<Mutex<PoolInner>>, timeout: std::time::Duration) {
    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Register waker via poll_fn, then sleep as timeout fallback.
    // The key improvement over 5ms spin: release() wakes us immediately
    // via the waker, and we only sleep up to `timeout` (not 5ms intervals).
    std::future::poll_fn(|cx| {
        if flag.swap(true, std::sync::atomic::Ordering::Relaxed) {
            // Second poll — we were woken (by release or timeout)
            return std::task::Poll::Ready(());
        }
        // First poll — register waker and yield
        let waker = cx.waker().clone();
        let inner2 = inner.clone();
        let flag2 = flag.clone();
        moduvex_runtime::spawn(async move {
            {
                let mut g = inner2.lock().await;
                g.waiters.push(waker.clone());
            }
            // Timeout fallback
            moduvex_runtime::sleep(timeout).await;
            flag2.store(true, std::sync::atomic::Ordering::Relaxed);
            waker.wake();
        });
        std::task::Poll::Pending
    })
    .await;
}

// ── URL parser ────────────────────────────────────────────────────────────────

/// Parse `postgres://user:password@host:port/database` into components.
///
/// Percent-decodes userinfo components to support special characters in passwords.
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
        .map(|(u, p)| (percent_decode(u), percent_decode(p)))
        .unwrap_or_else(|| (percent_decode(userinfo), String::new()));

    let (hostport, database) = hostpath
        .split_once('/')
        .ok_or_else(|| DbError::Other(format!("missing database name in URL: {url}")))?;

    Ok((user, password, hostport.to_string(), database.to_string()))
}

/// Decode percent-encoded string (e.g., "%40" → "@").
fn percent_decode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                result.push(byte as char);
                i += 3;
                continue;
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }

    result
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

    #[test]
    fn parse_url_percent_encoded_password() {
        let (user, pass, _, _) =
            parse_url("postgres://alice:p%40ssw%3Ard@127.0.0.1:5432/mydb").unwrap();
        assert_eq!(user, "alice");
        assert_eq!(pass, "p@ssw:rd");
    }

    // ── Additional pool/URL tests ──────────────────────────────────────────────

    #[test]
    fn parse_url_no_password() {
        // URL without ':password' part
        let (user, pass, hostport, db) =
            parse_url("postgres://alice@127.0.0.1:5432/mydb").unwrap();
        assert_eq!(user, "alice");
        assert_eq!(pass, "");
        assert_eq!(hostport, "127.0.0.1:5432");
        assert_eq!(db, "mydb");
    }

    #[test]
    fn parse_url_localhost_with_default_port() {
        let (_, _, hostport, db) =
            parse_url("postgres://user:pass@localhost:5432/testdb").unwrap();
        assert_eq!(hostport, "localhost:5432");
        assert_eq!(db, "testdb");
    }

    #[test]
    fn parse_url_percent_encoded_at_sign_in_password() {
        let (_, pass, _, _) =
            parse_url("postgres://user:pass%40word@localhost:5432/db").unwrap();
        assert_eq!(pass, "pass@word");
    }

    #[test]
    fn parse_url_percent_encoded_colon_in_password() {
        let (_, pass, _, _) =
            parse_url("postgres://user:pass%3Aword@localhost:5432/db").unwrap();
        assert_eq!(pass, "pass:word");
    }

    #[test]
    fn pool_construction_does_not_panic() {
        let cfg = PoolConfig::new("postgres://u:p@127.0.0.1:5432/db");
        // Just verify construction succeeds
        let pool = ConnectionPool::new(cfg);
        assert!(Arc::strong_count(&pool) >= 1);
    }

    #[test]
    fn pool_config_accessible_via_pool() {
        let cfg = PoolConfig::new("postgres://u:p@127.0.0.1:5432/mydb")
            .max_size(5);
        let pool = ConnectionPool::new(cfg);
        assert_eq!(pool.config().max_size, 5);
        assert_eq!(pool.config().database_url, "postgres://u:p@127.0.0.1:5432/mydb");
    }

    #[test]
    fn parse_url_empty_string_returns_error() {
        assert!(parse_url("").is_err());
    }

    #[test]
    fn parse_url_only_scheme_returns_error() {
        assert!(parse_url("postgres://").is_err());
    }
}

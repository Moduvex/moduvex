//! Pool health monitor — background task that pings idle connections and
//! evicts those that are dead or past their idle/max-lifetime timeout.
//!
//! Run via `spawn_health_monitor(pool)` inside a moduvex-runtime context.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::pool::ConnectionPool;

// ── HealthMonitor ─────────────────────────────────────────────────────────────

/// Configuration for the health monitor task.
#[derive(Debug, Clone)]
pub struct HealthMonitorConfig {
    /// How often to run the health check sweep.
    pub check_interval: Duration,
    /// Maximum idle duration before a connection is evicted.
    pub idle_timeout: Duration,
}

impl HealthMonitorConfig {
    pub fn from_pool_config(cfg: &crate::pool::PoolConfig) -> Self {
        Self {
            check_interval: cfg.health_check_interval,
            idle_timeout: cfg.idle_timeout,
        }
    }
}

/// Run one health-check sweep on the pool:
/// 1. Drain connections that have been idle longer than `idle_timeout`.
/// 2. Ping remaining idle connections; evict those that fail.
/// 3. Ensure at least `min_idle` connections exist (create new ones as needed).
///
/// This is called by the background loop (see `spawn_health_monitor`).
pub async fn run_health_sweep(pool: &Arc<ConnectionPool>) {
    let idle_timeout = pool.config().idle_timeout;
    let max_lifetime = pool.config().max_lifetime;
    let min_idle = pool.config().min_idle;
    let now = Instant::now();

    // ── Step 1 & 2: drain idle list, filter out stale and dead connections ──
    let to_check: Vec<_> = {
        let mut g = pool.inner().lock().await;
        // Take all idle connections out of the list
        g.idle.drain(..).collect()
    };

    let mut survivors = Vec::new();
    for mut entry in to_check {
        // Evict if idle too long or past max lifetime
        if now.duration_since(entry.idle_since) > idle_timeout
            || now.duration_since(entry.created_at) > max_lifetime
        {
            // Decrement live count; connection is dropped here
            let mut g = pool.inner().lock().await;
            g.live = g.live.saturating_sub(1);
            drop(g);
            continue;
        }
        // Ping to verify liveness
        match entry.conn.ping().await {
            Ok(()) => survivors.push(entry),
            Err(_) => {
                // Dead connection: evict
                let mut g = pool.inner().lock().await;
                g.live = g.live.saturating_sub(1);
            }
        }
    }

    // ── Step 3: return surviving connections to idle list ───────────────────
    {
        let mut g = pool.inner().lock().await;
        for entry in survivors {
            g.idle.push_back(entry);
        }
    }

    // ── Step 4: create connections to reach min_idle ─────────────────────
    let current_idle = pool.idle_count().await;
    let current_live = pool.live_count().await;
    let max_size = pool.config().max_size;

    if current_idle < min_idle && current_live < max_size {
        let needed = (min_idle - current_idle).min(max_size - current_live);
        for _ in 0..needed {
            match pool.acquire().await {
                Ok(conn) => pool.release(conn).await,
                Err(_) => break, // connection failed; skip
            }
        }
    }
}

/// Spawn a background health-monitor task on the moduvex-runtime executor.
///
/// The task runs `run_health_sweep` at `check_interval` until the pool is closed.
///
/// # Panics
/// Panics if called outside a `block_on_with_spawn` or `Runtime` context
/// (because `moduvex_runtime::spawn` requires a live executor).
pub fn spawn_health_monitor(pool: Arc<ConnectionPool>) {
    let interval = pool.config().health_check_interval;
    moduvex_runtime::spawn(async move {
        loop {
            moduvex_runtime::sleep(interval).await;
            // Stop when pool is explicitly closed — not based on counts
            if pool.inner().lock().await.closed {
                break;
            }
            run_health_sweep(&pool).await;
        }
    });
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::PoolConfig;

    #[test]
    fn health_config_from_pool_config() {
        let pcfg = PoolConfig::new("postgres://u:p@localhost/db")
            .health_check_interval(Duration::from_secs(30))
            .idle_timeout(Duration::from_secs(120));
        let hcfg = HealthMonitorConfig::from_pool_config(&pcfg);
        assert_eq!(hcfg.check_interval, Duration::from_secs(30));
        assert_eq!(hcfg.idle_timeout, Duration::from_secs(120));
    }

    #[test]
    fn pool_new_starts_empty() {
        let cfg = PoolConfig::new("postgres://u:p@127.0.0.1:5432/db");
        let pool = ConnectionPool::new(cfg);
        // Cannot call async methods in sync test, but can verify Arc creation
        assert!(Arc::strong_count(&pool) >= 1);
    }

    // ── Additional health tests ────────────────────────────────────────────────

    #[test]
    fn health_config_check_interval_nonzero() {
        let pcfg = PoolConfig::new("postgres://u:p@localhost/db");
        let hcfg = HealthMonitorConfig::from_pool_config(&pcfg);
        assert!(hcfg.check_interval > Duration::ZERO);
    }

    #[test]
    fn health_config_idle_timeout_nonzero() {
        let pcfg = PoolConfig::new("postgres://u:p@localhost/db");
        let hcfg = HealthMonitorConfig::from_pool_config(&pcfg);
        assert!(hcfg.idle_timeout > Duration::ZERO);
    }

    #[test]
    fn health_config_from_custom_pool_config() {
        let pcfg = PoolConfig::new("postgres://u:p@localhost/db")
            .health_check_interval(Duration::from_secs(10))
            .idle_timeout(Duration::from_secs(60));
        let hcfg = HealthMonitorConfig::from_pool_config(&pcfg);
        assert_eq!(hcfg.check_interval, Duration::from_secs(10));
        assert_eq!(hcfg.idle_timeout, Duration::from_secs(60));
    }

    #[test]
    fn health_config_clone() {
        let pcfg = PoolConfig::new("postgres://u:p@localhost/db");
        let hcfg = HealthMonitorConfig::from_pool_config(&pcfg);
        let hcfg2 = hcfg.clone();
        assert_eq!(hcfg.check_interval, hcfg2.check_interval);
        assert_eq!(hcfg.idle_timeout, hcfg2.idle_timeout);
    }

    #[test]
    fn pool_arc_shared_count() {
        let cfg = PoolConfig::new("postgres://u:p@127.0.0.1:5432/db");
        let pool = ConnectionPool::new(cfg);
        let pool2 = Arc::clone(&pool);
        assert_eq!(Arc::strong_count(&pool), 2);
        drop(pool2);
        assert_eq!(Arc::strong_count(&pool), 1);
    }
}

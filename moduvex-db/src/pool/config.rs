//! `PoolConfig` — connection pool tuning parameters.

use std::time::Duration;

// ── PoolConfig ────────────────────────────────────────────────────────────────

/// Configuration for a `ConnectionPool`.
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// Minimum number of idle connections to maintain at all times.
    pub min_idle: usize,
    /// Maximum total connections (idle + checked-out) the pool may hold.
    pub max_size: usize,
    /// How long to wait for a connection to become available before timing out.
    pub connect_timeout: Duration,
    /// Maximum time a connection may sit idle before being evicted.
    pub idle_timeout: Duration,
    /// Maximum lifetime of any connection regardless of idle/active status.
    pub max_lifetime: Duration,
    /// Interval between health-check pings on idle connections.
    pub health_check_interval: Duration,
    /// PostgreSQL connection string, e.g. `"postgres://user:pass@host:5432/db"`.
    pub database_url: String,
}

impl PoolConfig {
    /// Construct with sensible defaults.
    ///
    /// * `min_idle` = 1
    /// * `max_size` = 10
    /// * `connect_timeout` = 30 s
    /// * `idle_timeout` = 10 min
    /// * `max_lifetime` = 30 min
    /// * `health_check_interval` = 60 s
    pub fn new(database_url: impl Into<String>) -> Self {
        Self {
            min_idle: 1,
            max_size: 10,
            connect_timeout: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(600),
            max_lifetime: Duration::from_secs(1800),
            health_check_interval: Duration::from_secs(60),
            database_url: database_url.into(),
        }
    }

    /// Override `min_idle`.
    pub fn min_idle(mut self, n: usize) -> Self {
        self.min_idle = n;
        self
    }

    /// Override `max_size`.
    pub fn max_size(mut self, n: usize) -> Self {
        self.max_size = n;
        self
    }

    /// Override `connect_timeout`.
    pub fn connect_timeout(mut self, d: Duration) -> Self {
        self.connect_timeout = d;
        self
    }

    /// Override `idle_timeout`.
    pub fn idle_timeout(mut self, d: Duration) -> Self {
        self.idle_timeout = d;
        self
    }

    /// Override `max_lifetime`.
    pub fn max_lifetime(mut self, d: Duration) -> Self {
        self.max_lifetime = d;
        self
    }

    /// Override `health_check_interval`.
    pub fn health_check_interval(mut self, d: Duration) -> Self {
        self.health_check_interval = d;
        self
    }

    /// Validate the configuration; returns `Err` with a description if invalid.
    pub fn validate(&self) -> Result<(), String> {
        if self.max_size == 0 {
            return Err("max_size must be > 0".into());
        }
        if self.min_idle > self.max_size {
            return Err(format!(
                "min_idle ({}) must be <= max_size ({})",
                self.min_idle, self.max_size
            ));
        }
        if self.database_url.is_empty() {
            return Err("database_url must not be empty".into());
        }
        Ok(())
    }
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self::new("postgres://localhost/postgres")
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        let cfg = PoolConfig::new("postgres://localhost/test");
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn max_size_zero_is_invalid() {
        let cfg = PoolConfig::new("postgres://localhost/test").max_size(0);
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn min_idle_exceeds_max_is_invalid() {
        let cfg = PoolConfig::new("postgres://localhost/test")
            .max_size(3)
            .min_idle(5);
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn empty_url_is_invalid() {
        let cfg = PoolConfig::new("");
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn builder_methods_chain() {
        let cfg = PoolConfig::new("postgres://localhost/test")
            .min_idle(2)
            .max_size(20)
            .connect_timeout(Duration::from_secs(5))
            .idle_timeout(Duration::from_secs(300));
        assert_eq!(cfg.min_idle, 2);
        assert_eq!(cfg.max_size, 20);
        assert_eq!(cfg.connect_timeout, Duration::from_secs(5));
        assert_eq!(cfg.idle_timeout, Duration::from_secs(300));
        assert!(cfg.validate().is_ok());
    }

    // ── Additional pool config tests ───────────────────────────────────────────

    #[test]
    fn pool_config_default_max_size_is_reasonable() {
        let cfg = PoolConfig::default();
        assert!(cfg.max_size >= 1, "default max_size must be >= 1");
    }

    #[test]
    fn pool_config_validation_zero_max_size_fails() {
        let mut cfg = PoolConfig::default();
        cfg.max_size = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn pool_config_validation_zero_min_idle_ok() {
        let mut cfg = PoolConfig::default();
        cfg.min_idle = 0;
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn pool_config_connection_string_set() {
        let cfg = PoolConfig::new("postgres://localhost/test");
        assert!(cfg.database_url.contains("postgres"));
    }

    #[test]
    fn pool_config_max_lifetime_default_nonzero() {
        let cfg = PoolConfig::default();
        assert!(cfg.max_lifetime > Duration::ZERO);
    }

    #[test]
    fn pool_config_health_check_interval_default_nonzero() {
        let cfg = PoolConfig::default();
        assert!(cfg.health_check_interval > Duration::ZERO);
    }

    #[test]
    fn pool_config_max_lifetime_override() {
        let cfg = PoolConfig::new("postgres://localhost/test")
            .max_lifetime(Duration::from_secs(900));
        assert_eq!(cfg.max_lifetime, Duration::from_secs(900));
    }

    #[test]
    fn pool_config_health_check_interval_override() {
        let cfg = PoolConfig::new("postgres://localhost/test")
            .health_check_interval(Duration::from_secs(15));
        assert_eq!(cfg.health_check_interval, Duration::from_secs(15));
    }

    #[test]
    fn pool_config_min_idle_equals_max_size_is_ok() {
        let cfg = PoolConfig::new("postgres://localhost/test")
            .max_size(5)
            .min_idle(5);
        assert!(cfg.validate().is_ok());
    }
}

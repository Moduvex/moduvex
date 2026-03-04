//! Health check system: trait-based checks with aggregated status.

use std::sync::Mutex;

/// Overall health status of a component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthStatus {
    Healthy,
    Degraded(String),
    Unhealthy(String),
}

impl HealthStatus {
    /// Whether this status is considered healthy.
    pub fn is_healthy(&self) -> bool {
        matches!(self, Self::Healthy)
    }
}

impl std::fmt::Display for HealthStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Healthy => write!(f, "healthy"),
            Self::Degraded(msg) => write!(f, "degraded: {msg}"),
            Self::Unhealthy(msg) => write!(f, "unhealthy: {msg}"),
        }
    }
}

/// Result of a single health check.
#[derive(Debug, Clone)]
pub struct CheckResult {
    pub name: String,
    pub status: HealthStatus,
}

/// Trait for components that can report their health.
pub trait HealthCheck: Send + Sync {
    /// Human-readable name of this check.
    fn name(&self) -> &str;

    /// Perform the health check (synchronous version).
    fn check(&self) -> HealthStatus;
}

/// Registry that aggregates multiple health checks.
pub struct HealthRegistry {
    checks: Mutex<Vec<Box<dyn HealthCheck>>>,
}

impl HealthRegistry {
    pub fn new() -> Self {
        Self {
            checks: Mutex::new(Vec::new()),
        }
    }

    /// Register a health check.
    pub fn register(&self, check: impl HealthCheck + 'static) {
        self.checks.lock().unwrap().push(Box::new(check));
    }

    /// Run all checks and return individual results.
    pub fn check_all(&self) -> Vec<CheckResult> {
        let checks = self.checks.lock().unwrap();
        checks
            .iter()
            .map(|c| CheckResult {
                name: c.name().to_owned(),
                status: c.check(),
            })
            .collect()
    }

    /// Aggregate status: Unhealthy if any unhealthy, Degraded if any degraded.
    pub fn aggregate(&self) -> HealthStatus {
        let results = self.check_all();
        let mut worst = HealthStatus::Healthy;
        for r in &results {
            match &r.status {
                HealthStatus::Unhealthy(msg) => {
                    return HealthStatus::Unhealthy(format!("{}: {msg}", r.name));
                }
                HealthStatus::Degraded(msg) => {
                    worst = HealthStatus::Degraded(format!("{}: {msg}", r.name));
                }
                HealthStatus::Healthy => {}
            }
        }
        worst
    }
}

impl Default for HealthRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct AlwaysHealthy;
    impl HealthCheck for AlwaysHealthy {
        fn name(&self) -> &str {
            "always_healthy"
        }
        fn check(&self) -> HealthStatus {
            HealthStatus::Healthy
        }
    }

    struct AlwaysUnhealthy;
    impl HealthCheck for AlwaysUnhealthy {
        fn name(&self) -> &str {
            "always_unhealthy"
        }
        fn check(&self) -> HealthStatus {
            HealthStatus::Unhealthy("down".into())
        }
    }

    struct DegradedCheck;
    impl HealthCheck for DegradedCheck {
        fn name(&self) -> &str {
            "degraded"
        }
        fn check(&self) -> HealthStatus {
            HealthStatus::Degraded("slow".into())
        }
    }

    #[test]
    fn all_healthy() {
        let reg = HealthRegistry::new();
        reg.register(AlwaysHealthy);
        assert_eq!(reg.aggregate(), HealthStatus::Healthy);
    }

    #[test]
    fn one_unhealthy_makes_aggregate_unhealthy() {
        let reg = HealthRegistry::new();
        reg.register(AlwaysHealthy);
        reg.register(AlwaysUnhealthy);
        assert!(matches!(reg.aggregate(), HealthStatus::Unhealthy(_)));
    }

    #[test]
    fn degraded_propagates() {
        let reg = HealthRegistry::new();
        reg.register(AlwaysHealthy);
        reg.register(DegradedCheck);
        assert!(matches!(reg.aggregate(), HealthStatus::Degraded(_)));
    }

    #[test]
    fn check_all_returns_individual_results() {
        let reg = HealthRegistry::new();
        reg.register(AlwaysHealthy);
        reg.register(DegradedCheck);
        let results = reg.check_all();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].name, "always_healthy");
        assert!(results[0].status.is_healthy());
    }
}

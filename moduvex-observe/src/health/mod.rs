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

/// Trait for components that can report their health synchronously.
pub trait HealthCheck: Send + Sync {
    /// Human-readable name of this check.
    fn name(&self) -> &str;

    /// Perform the health check.
    fn check(&self) -> HealthStatus;
}

/// Trait for components that require async to report health (e.g. DB pings).
///
/// Use this for checks that need I/O (network, disk) via the async runtime.
/// Returns a `'static` future so it can be collected and awaited outside a lock.
pub trait AsyncHealthCheck: Send + Sync {
    /// Human-readable name of this check.
    fn name(&self) -> &str;

    /// Perform the health check asynchronously.
    fn check(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = HealthStatus> + Send>>;
}

/// Registry that aggregates multiple health checks (sync and async).
pub struct HealthRegistry {
    sync_checks: Mutex<Vec<Box<dyn HealthCheck>>>,
    async_checks: Mutex<Vec<Box<dyn AsyncHealthCheck>>>,
}

impl HealthRegistry {
    pub fn new() -> Self {
        Self {
            sync_checks: Mutex::new(Vec::new()),
            async_checks: Mutex::new(Vec::new()),
        }
    }

    /// Register a synchronous health check.
    pub fn register(&self, check: impl HealthCheck + 'static) {
        self.sync_checks.lock().unwrap().push(Box::new(check));
    }

    /// Register an async health check (e.g. DB connection ping).
    pub fn register_async(&self, check: Box<dyn AsyncHealthCheck>) {
        self.async_checks.lock().unwrap().push(check);
    }

    /// Run all synchronous checks and return individual results.
    pub fn check_all(&self) -> Vec<CheckResult> {
        let checks = self.sync_checks.lock().unwrap();
        checks
            .iter()
            .map(|c| CheckResult {
                name: c.name().to_owned(),
                status: c.check(),
            })
            .collect()
    }

    /// Run all checks (sync + async) and return individual results.
    pub async fn check_all_async(&self) -> Vec<CheckResult> {
        let mut results = self.check_all();

        // Collect async check references while holding the lock, then run outside lock
        let names_and_futures: Vec<_> = {
            let async_checks = self.async_checks.lock().unwrap();
            async_checks
                .iter()
                .map(|c| (c.name().to_owned(), c.check()))
                .collect()
        };

        for (name, future) in names_and_futures {
            let status = future.await;
            results.push(CheckResult { name, status });
        }

        results
    }

    /// Aggregate status: Unhealthy if any unhealthy, Degraded if any degraded.
    pub fn aggregate(&self) -> HealthStatus {
        let results = self.check_all();
        aggregate_results(&results)
    }

    /// Aggregate all checks including async ones.
    pub async fn aggregate_async(&self) -> HealthStatus {
        let results = self.check_all_async().await;
        aggregate_results(&results)
    }
}

/// Compute worst status from a list of check results.
fn aggregate_results(results: &[CheckResult]) -> HealthStatus {
    let mut worst = HealthStatus::Healthy;
    for r in results {
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

    #[test]
    fn empty_registry_aggregate_is_healthy() {
        let reg = HealthRegistry::new();
        assert_eq!(reg.aggregate(), HealthStatus::Healthy);
    }

    #[test]
    fn empty_registry_check_all_returns_empty() {
        let reg = HealthRegistry::new();
        let results = reg.check_all();
        assert!(results.is_empty());
    }

    #[test]
    fn health_status_is_healthy_only_for_healthy_variant() {
        assert!(HealthStatus::Healthy.is_healthy());
        assert!(!HealthStatus::Degraded("x".into()).is_healthy());
        assert!(!HealthStatus::Unhealthy("x".into()).is_healthy());
    }

    #[test]
    fn health_status_display_healthy() {
        assert_eq!(HealthStatus::Healthy.to_string(), "healthy");
    }

    #[test]
    fn health_status_display_degraded() {
        let s = HealthStatus::Degraded("disk full".into()).to_string();
        assert_eq!(s, "degraded: disk full");
    }

    #[test]
    fn health_status_display_unhealthy() {
        let s = HealthStatus::Unhealthy("timeout".into()).to_string();
        assert_eq!(s, "unhealthy: timeout");
    }

    #[test]
    fn aggregate_unhealthy_message_includes_check_name() {
        let reg = HealthRegistry::new();
        reg.register(AlwaysUnhealthy);
        match reg.aggregate() {
            HealthStatus::Unhealthy(msg) => assert!(msg.contains("always_unhealthy")),
            _ => panic!("expected unhealthy"),
        }
    }

    #[test]
    fn aggregate_degraded_message_includes_check_name() {
        let reg = HealthRegistry::new();
        reg.register(DegradedCheck);
        match reg.aggregate() {
            HealthStatus::Degraded(msg) => assert!(msg.contains("degraded")),
            _ => panic!("expected degraded"),
        }
    }

    #[test]
    fn unhealthy_takes_priority_over_degraded() {
        let reg = HealthRegistry::new();
        reg.register(DegradedCheck);
        reg.register(AlwaysUnhealthy);
        assert!(matches!(reg.aggregate(), HealthStatus::Unhealthy(_)));
    }

    #[test]
    fn check_result_name_matches_registered_check() {
        let reg = HealthRegistry::new();
        reg.register(AlwaysHealthy);
        let results = reg.check_all();
        assert_eq!(results[0].name, "always_healthy");
    }

    #[test]
    fn health_status_equality() {
        assert_eq!(HealthStatus::Healthy, HealthStatus::Healthy);
        assert_ne!(HealthStatus::Healthy, HealthStatus::Degraded("x".into()));
        assert_eq!(
            HealthStatus::Unhealthy("err".into()),
            HealthStatus::Unhealthy("err".into())
        );
    }

    #[test]
    fn registry_default_equals_new() {
        let reg = HealthRegistry::default();
        assert!(reg.check_all().is_empty());
    }

    #[test]
    fn multiple_checks_all_healthy() {
        let reg = HealthRegistry::new();
        reg.register(AlwaysHealthy);
        reg.register(AlwaysHealthy);
        reg.register(AlwaysHealthy);
        assert_eq!(reg.aggregate(), HealthStatus::Healthy);
        assert_eq!(reg.check_all().len(), 3);
    }
}

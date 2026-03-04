//! Module trait family — the building blocks of a Moduvex application.
//!
//! Every feature is expressed as a `Module`. Modules declare their config
//! type, their lifecycle hooks, and their route contributions.
//!
//! # Trait hierarchy
//! ```text
//! Module              — identity (name, priority)
//!   ModuleConfig      — declares a typed config struct
//!   ModuleLifecycle   — async on_start / on_stop hooks
//!   ModuleRoutes      — contributes HTTP routes via RouteSink
//! ```

pub mod dependency;
pub mod registry;

pub use dependency::{AllDepsOk, ContainsAll, ContainsModule, DependsOn, Here, There};

use std::future::Future;
use std::pin::Pin;

use crate::app::context::AppContext;
use crate::error::Result;

// ── Module ────────────────────────────────────────────────────────────────────

/// Core identity trait — every module must implement this.
///
/// Modules are value types (structs) that implement this trait. They are
/// registered at compile time via the type-state builder and instantiated
/// once during the `Init` lifecycle phase.
pub trait Module: Send + Sync + 'static {
    /// The module's human-readable name, used in logs and error messages.
    fn name(&self) -> &'static str;

    /// Startup priority within the same dependency level (higher = earlier).
    ///
    /// Modules at the same depth in the dependency graph are sorted by this
    /// value (descending) before their hooks are called.
    fn priority(&self) -> i32 {
        0
    }
}

// ── ModuleLifecycle ───────────────────────────────────────────────────────────

/// Lifecycle hooks called by the `LifecycleEngine`.
///
/// Implement this trait to run async initialization and cleanup logic.
/// `on_start` is called during the `Start` phase; `on_stop` during `Stopping`.
///
/// Both methods receive the shared `AppContext` so they can register or
/// resolve singletons.
///
/// # Boxed future
/// We use `Pin<Box<dyn Future>>` (manual async-compatible object-safety) so
/// that `ModuleLifecycle` trait objects can be stored in `Vec<Box<dyn …>>`.
/// This avoids the `async-trait` proc-macro while keeping the public API
/// clean — users implement normal `async fn` via the blanket below.
pub trait ModuleLifecycle: Module {
    /// Called in dependency order during the `Start` phase.
    fn on_start<'a>(
        &'a self,
        ctx: &'a AppContext,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

    /// Called in reverse dependency order during the `Stopping` phase.
    fn on_stop<'a>(
        &'a self,
        ctx: &'a AppContext,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
}

// ── RouteSink ─────────────────────────────────────────────────────────────────

/// Object-safe sink for route registrations.
///
/// `moduvex-http` provides a concrete implementation; `moduvex-core` only
/// depends on this trait — it does not depend on the HTTP crate.
pub trait RouteSink {
    /// Register a route with the given HTTP method, path, and erased handler.
    fn add_route(&mut self, method: &str, path: &str, handler: Box<dyn ErasedHandler>);
}

/// An object-safe handler that processes an erased request.
///
/// Concrete implementations live in `moduvex-http`; this trait is a
/// forwarding abstraction so `moduvex-core` stays HTTP-crate-agnostic.
pub trait ErasedHandler: Send + Sync + 'static {}

// ── ModuleRoutes ──────────────────────────────────────────────────────────────

/// Contributes HTTP routes to the application router.
///
/// Modules that handle HTTP requests implement this trait and register their
/// routes against the provided [`RouteSink`].
pub trait ModuleRoutes: Module {
    /// Register all routes provided by this module.
    fn register_routes(&self, router: &mut dyn RouteSink);
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyModule;

    impl Module for DummyModule {
        fn name(&self) -> &'static str {
            "dummy"
        }
    }

    #[test]
    fn module_name_and_default_priority() {
        let m = DummyModule;
        assert_eq!(m.name(), "dummy");
        assert_eq!(m.priority(), 0);
    }

    struct HighPriorityModule;
    impl Module for HighPriorityModule {
        fn name(&self) -> &'static str {
            "high"
        }
        fn priority(&self) -> i32 {
            100
        }
    }

    #[test]
    fn module_custom_priority() {
        assert_eq!(HighPriorityModule.priority(), 100);
    }
}

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

    /// Runtime dependency names — module names this module depends on.
    ///
    /// Returns the names of modules that must be started before this one.
    /// Used by `ModuleRegistry::topological_sort()` to establish correct boot
    /// order. Defaults to empty (no runtime dependencies declared).
    ///
    /// If your module uses compile-time `DependsOn`, you should mirror the
    /// dependency names here so the runtime topological sort is aware of them:
    ///
    /// ```rust,ignore
    /// fn dep_names(&self) -> Vec<&'static str> {
    ///     vec!["DatabaseModule"]
    /// }
    /// ```
    fn dep_names(&self) -> Vec<&'static str> {
        vec![]
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

    #[test]
    fn module_default_dep_names_empty() {
        let m = DummyModule;
        assert!(m.dep_names().is_empty());
    }

    #[test]
    fn module_with_dep_names() {
        struct DepModule;
        impl Module for DepModule {
            fn name(&self) -> &'static str { "dep-module" }
            fn dep_names(&self) -> Vec<&'static str> { vec!["auth", "db"] }
        }

        let m = DepModule;
        let deps = m.dep_names();
        assert_eq!(deps, vec!["auth", "db"]);
    }

    #[test]
    fn module_lifecycle_on_start_and_stop() {
        use crate::app::context::AppContext;
        use crate::error::Result;
        use std::future::Future;
        use std::pin::Pin;
        use std::sync::{Arc, Mutex};

        struct CountModule {
            starts: Arc<Mutex<u32>>,
            stops: Arc<Mutex<u32>>,
        }

        impl Module for CountModule {
            fn name(&self) -> &'static str { "counter" }
        }

        impl ModuleLifecycle for CountModule {
            fn on_start<'a>(
                &'a self,
                _ctx: &'a AppContext,
            ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
                let starts = Arc::clone(&self.starts);
                Box::pin(async move {
                    *starts.lock().unwrap() += 1;
                    Ok(())
                })
            }

            fn on_stop<'a>(
                &'a self,
                _ctx: &'a AppContext,
            ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
                let stops = Arc::clone(&self.stops);
                Box::pin(async move {
                    *stops.lock().unwrap() += 1;
                    Ok(())
                })
            }
        }

        let starts = Arc::new(Mutex::new(0u32));
        let stops = Arc::new(Mutex::new(0u32));
        let m = CountModule { starts: Arc::clone(&starts), stops: Arc::clone(&stops) };
        let ctx = AppContext::new();

        moduvex_runtime::block_on(async {
            m.on_start(&ctx).await.unwrap();
            m.on_start(&ctx).await.unwrap();
            m.on_stop(&ctx).await.unwrap();
        });

        assert_eq!(*starts.lock().unwrap(), 2);
        assert_eq!(*stops.lock().unwrap(), 1);
    }

    #[test]
    fn negative_priority_module() {
        struct LowPriority;
        impl Module for LowPriority {
            fn name(&self) -> &'static str { "low" }
            fn priority(&self) -> i32 { -100 }
        }

        assert_eq!(LowPriority.priority(), -100);
    }
}

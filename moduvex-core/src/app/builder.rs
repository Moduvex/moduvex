//! Type-state builder for the Moduvex application.
//!
//! The builder encodes the application state and registered module list in the
//! type system via `PhantomData`. This means:
//! - Calling `.run()` without `.config()` first is a **compile error**.
//! - Calling `.run()` with unsatisfied module dependencies is a **compile error**.
//! - Zero runtime overhead for these checks — all erased after monomorphisation.
//!
//! # Type-list encoding
//! Modules are prepended to a nested-tuple list:
//! ```text
//! ()                   — no modules
//! (A, ())              — module A registered
//! (B, (A, ()))         — modules A, B registered (B prepended last)
//! ```
//!
//! # Recursion limit
//! The trait solver recurses O(n) deep for `AllDependenciesSatisfied`. With
//! Rust's default limit of 128 the builder supports ~60 modules. To raise the
//! limit add `#![recursion_limit = "256"]` to your crate root.

use std::marker::PhantomData;
use std::sync::Arc;

use crate::app::context::AppContext;
use crate::app::state::{Configured, Unconfigured};
use crate::error::Result;
use crate::lifecycle::{LifecycleEngine, ShutdownConfig};
use crate::module::registry::{ModuleEntry, ModuleRegistry};
use crate::module::{AllDepsOk, DependsOn, Module, ModuleLifecycle};

// ── Moduvex ──────────────────────────────────────────────────────────────────

/// The top-level application builder.
///
/// `State` is one of [`Unconfigured`] or [`Configured`].
/// `Modules` is a nested-tuple type-list of registered module types.
///
/// Users never name these type parameters directly — the builder methods
/// return the correctly-typed next step.
pub struct Moduvex<State, Modules = ()> {
    /// Registered module entries (runtime backing for lifecycle calls).
    module_entries: Vec<ModuleEntry>,
    /// Optional shutdown configuration.
    shutdown_cfg: Option<ShutdownConfig>,
    _state: PhantomData<State>,
    _modules: PhantomData<Modules>,
}

// ── Unconfigured state ────────────────────────────────────────────────────────

impl Moduvex<Unconfigured, ()> {
    /// Create a new, unconfigured application builder.
    pub fn new() -> Self {
        Self {
            module_entries: Vec::new(),
            shutdown_cfg: None,
            _state: PhantomData,
            _modules: PhantomData,
        }
    }

    /// Provide configuration, advancing the builder to `Configured`.
    ///
    /// `path` is reserved for future config-file loading (Phase 5 —
    /// moduvex-config). Currently this method simply transitions the
    /// type-state without performing I/O.
    pub fn config(self, _path: &str) -> Moduvex<Configured, ()> {
        Moduvex {
            module_entries: self.module_entries,
            shutdown_cfg: self.shutdown_cfg,
            _state: PhantomData,
            _modules: PhantomData,
        }
    }
}

impl Default for Moduvex<Unconfigured, ()> {
    fn default() -> Self {
        Self::new()
    }
}

// ── Configured state — module registration ────────────────────────────────────

impl<Modules> Moduvex<Configured, Modules> {
    /// Register module `M`, prepending it to the module type-list.
    ///
    /// The module must implement both [`Module`] and [`ModuleLifecycle`] so
    /// the engine can call its lifecycle hooks at runtime.
    ///
    /// Dependency checking (`DependsOn`) is enforced at `.run()` time, not
    /// here, so you can register modules in any order.
    pub fn module<M>(mut self, instance: M) -> Moduvex<Configured, (M, Modules)>
    where
        M: Module + ModuleLifecycle + DependsOn,
    {
        let entry = ModuleEntry {
            name: instance.name(),
            priority: instance.priority(),
            deps: instance.dep_names(),
            lifecycle: Box::new(instance),
        };
        self.module_entries.push(entry);

        Moduvex {
            module_entries: self.module_entries,
            shutdown_cfg: self.shutdown_cfg,
            _state: PhantomData,
            _modules: PhantomData,
        }
    }

    /// Override the default shutdown configuration.
    pub fn shutdown_config(mut self, cfg: ShutdownConfig) -> Self {
        self.shutdown_cfg = Some(cfg);
        self
    }
}

// ── .run() — only when all deps satisfied ─────────────────────────────────────

impl<Modules> Moduvex<Configured, Modules> {
    /// Boot the application and run until a shutdown signal is received.
    ///
    /// This method is only callable when every registered module's declared
    /// dependencies are also registered. A missing dependency is a **compile
    /// error** — enforced by the `AllDepsOk<Proofs>` trait bound on the
    /// method's free type parameter `Proofs` (inferred by the compiler).
    ///
    /// Internally:
    /// 1. Builds a [`ModuleRegistry`] in priority order.
    /// 2. Creates a fresh [`AppContext`].
    /// 3. Hands both to the [`LifecycleEngine`] and awaits completion.
    pub async fn run<Proofs>(self) -> Result<()>
    where
        Modules: AllDepsOk<Proofs>,
    {
        let mut registry = ModuleRegistry::new();
        for entry in self.module_entries {
            registry.push(entry);
        }
        registry.sort_by_priority();

        let ctx = Arc::new(AppContext::new());
        let mut engine = LifecycleEngine::new(registry, ctx);

        if let Some(cfg) = self.shutdown_cfg {
            engine = engine.with_shutdown_config(cfg);
        }

        engine.run().await
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::context::AppContext;
    use crate::error::Result;
    use std::future::Future;
    use std::pin::Pin;

    // ── Minimal test module ──────────────────────────────────────────────────

    struct NoopModule;

    impl Module for NoopModule {
        fn name(&self) -> &'static str {
            "noop"
        }
    }

    impl DependsOn for NoopModule {
        type Required = ();
    }

    impl ModuleLifecycle for NoopModule {
        fn on_start<'a>(
            &'a self,
            _ctx: &'a AppContext,
        ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
            Box::pin(async { Ok(()) })
        }
        fn on_stop<'a>(
            &'a self,
            _ctx: &'a AppContext,
        ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[test]
    fn builder_new_creates_unconfigured() {
        let _b = Moduvex::new();
        // Compiles = Unconfigured state exists.
    }

    #[test]
    fn config_transitions_to_configured() {
        let _b = Moduvex::new().config("app.toml");
        // Compiles = Configured state is reachable.
    }

    #[test]
    fn module_registration_compiles() {
        let _b = Moduvex::new().config("app.toml").module(NoopModule);
        // Compiles = module<M>() returns correct type.
    }

    #[test]
    fn run_with_no_dep_module_completes() {
        // NoopModule has no deps so AllDependenciesSatisfied is trivially true.
        moduvex_runtime::block_on(async {
            {
                // We need to trigger shutdown programmatically.
                // Build engine manually so we can grab the handle.
                let mut registry = ModuleRegistry::new();
                registry.push(ModuleEntry {
                    name: "noop",
                    priority: 0,
                    deps: vec![],
                    lifecycle: Box::new(NoopModule),
                });
                let ctx = Arc::new(AppContext::new());
                let engine = LifecycleEngine::new(registry, ctx);
                let h = engine.shutdown_handle();
                h.request(); // immediate shutdown
                let result = engine.run().await;
                assert!(result.is_ok());
            };
        });
    }

    // NOTE: compile-fail test (missing dep = compile error) lives in
    // tests/compile_fail/missing_dep.rs and is driven by `trybuild`.
}

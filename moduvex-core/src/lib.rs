//! `moduvex-core` — Type-state DI, module system, and lifecycle engine.
//!
//! This crate is the brain of the Moduvex framework. It provides:
//!
//! - **Type-state builder** — `Moduvex::new().config().module::<M>().run()`
//!   with compile-time dependency validation (missing dep = compile error).
//! - **Module trait family** — `Module`, `ModuleLifecycle`, `ModuleRoutes`
//! - **Lifecycle engine** — deterministic 7-phase boot: Config → Validate →
//!   Init → Start → Ready → Stopping → Stopped with rollback on failure.
//! - **DI system** — `TypeMap`-backed `AppContext` for singletons, plus
//!   `RequestScoped<T>` factory pattern for per-request values.
//! - **Error system** — `ModuvexError` with Domain/Infra/Config/Lifecycle
//!   classification and context-chaining extension trait.
//! - **Transaction boundary** — `TransactionBoundary` stub (implemented by
//!   `moduvex-db` in Phase 5).
//!
//! # Quick start
//! ```rust,no_run
//! use moduvex_core::prelude::*;
//!
//! struct MyModule;
//!
//! impl Module for MyModule {
//!     fn name(&self) -> &'static str { "my-module" }
//! }
//!
//! impl DependsOn for MyModule {
//!     type Required = ();  // no dependencies
//! }
//! ```
//!
//! # Runtime cost note
//! The compile-time dependency graph validation is zero-cost (erased after
//! monomorphisation). Singleton storage in `AppContext` uses
//! `HashMap<TypeId, Box<dyn Any>>` — one heap allocation per singleton at
//! startup, then `Arc<T>` clones on the hot path (no `TypeId` lookup per request).

// ── Crate modules ─────────────────────────────────────────────────────────────

pub mod app;
pub mod di;
pub mod error;
pub mod lifecycle;
pub mod module;
pub mod tx;

// ── Top-level re-exports ──────────────────────────────────────────────────────

pub use app::{AppContext, Configured, Moduvex, RequestContext, Unconfigured};
pub use di::{Inject, Provider, RequestScoped, Singleton, TypeMap};
pub use error::chain::Context as ErrorContext;
pub use error::classify::{ConfigError, DomainError, InfraError, LifecycleError};
pub use error::{ModuvexError, Result};
pub use lifecycle::{
    HookRegistry, LifecycleEngine, LifecycleHook, Phase, ShutdownConfig, ShutdownHandle,
};
pub use module::{
    AllDepsOk, ContainsAll, ContainsModule, DependsOn, ErasedHandler, Here, Module,
    ModuleLifecycle, ModuleRoutes, RouteSink, There,
};
pub use tx::TransactionBoundary;

// ── Proc macro re-exports ───────────────────────────────────────────────────
// Users only need to depend on moduvex-core, not moduvex-macros directly.

pub use moduvex_macros::main as moduvex_main;
pub use moduvex_macros::module as moduvex_module;
pub use moduvex_macros::{Component, DomainError, InfraError, Module as DeriveModule};

// ── Prelude ───────────────────────────────────────────────────────────────────

/// Convenience re-export of the most commonly used items.
///
/// Add `use moduvex_core::prelude::*;` to your module files.
pub mod prelude {
    pub use crate::app::{AppContext, Moduvex, RequestContext};
    pub use crate::di::{Inject, Singleton};
    pub use crate::error::chain::Context as ErrorContext;
    pub use crate::error::{ModuvexError, Result};
    pub use crate::lifecycle::{Phase, ShutdownHandle};
    pub use crate::module::{AllDepsOk, DependsOn, Module, ModuleLifecycle, ModuleRoutes};
}

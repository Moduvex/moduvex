//! Lifecycle engine — orchestrates the 7-phase application boot/shutdown.
//!
//! The `LifecycleEngine` takes ownership of the `ModuleRegistry` and
//! `AppContext`, drives modules through Config → Validate → Init → Start →
//! Ready, then waits for a shutdown signal before reversing through Stopping →
//! Stopped.
//!
//! On any phase failure the engine rolls back by calling `on_stop()` on all
//! modules that successfully completed `on_start()`, in reverse order.

pub mod hook;
pub mod phase;
pub mod shutdown;

pub use hook::{HookRegistry, LifecycleHook};
pub use phase::Phase;
pub use shutdown::{wait_for_shutdown, ShutdownConfig, ShutdownHandle};

use std::sync::Arc;

use crate::app::context::AppContext;
use crate::error::{classify::LifecycleError, ModuvexError, Result};
use crate::module::registry::ModuleRegistry;

// ── LifecycleEngine ───────────────────────────────────────────────────────────

/// Orchestrates the full application lifecycle.
///
/// Created by `Moduvex::run()` after the type-state builder validates the
/// module graph at compile time. The engine handles the runtime sequence.
pub struct LifecycleEngine {
    registry: ModuleRegistry,
    ctx: Arc<AppContext>,
    hooks: HookRegistry,
    shutdown_cfg: ShutdownConfig,
    shutdown_handle: ShutdownHandle,
}

impl LifecycleEngine {
    /// Create a new engine with the given registry and context.
    pub fn new(registry: ModuleRegistry, ctx: Arc<AppContext>) -> Self {
        Self {
            registry,
            ctx,
            hooks: HookRegistry::new(),
            shutdown_cfg: ShutdownConfig::default(),
            shutdown_handle: ShutdownHandle::new(),
        }
    }

    /// Set a custom shutdown configuration.
    pub fn with_shutdown_config(mut self, cfg: ShutdownConfig) -> Self {
        self.shutdown_cfg = cfg;
        self
    }

    /// Register a framework-level lifecycle hook.
    pub fn add_hook(&mut self, hook: impl LifecycleHook) {
        self.hooks.register(hook);
    }

    /// Returns a cloneable handle for programmatic shutdown.
    pub fn shutdown_handle(&self) -> ShutdownHandle {
        self.shutdown_handle.clone()
    }

    /// Run the full lifecycle to completion.
    ///
    /// Returns `Ok(())` after a clean shutdown, or `Err` if any phase fails
    /// and rollback cannot complete cleanly (the original error is returned).
    pub async fn run(self) -> Result<()> {
        let LifecycleEngine {
            registry,
            ctx,
            hooks,
            shutdown_cfg: _,
            shutdown_handle,
        } = self;

        // ── Boot sequence ────────────────────────────────────────────────────
        // Config and Validate phases are currently stubs — real config loading
        // lives in moduvex-config (Phase 5). We advance through them to keep
        // the phase state machine honest.
        hooks.notify_phase_entered(Phase::Config, &ctx).await?;
        hooks.notify_phase_entered(Phase::Validate, &ctx).await?;
        hooks.notify_phase_entered(Phase::Init, &ctx).await?;

        // ── Start phase ──────────────────────────────────────────────────────
        // Call on_start() on each module in boot order.
        // Track how many modules started successfully for rollback purposes.
        let entries = registry.into_entries();

        for (idx, entry) in entries.iter().enumerate() {
            if let Err(e) = entry.lifecycle.on_start(&ctx).await {
                // Partial boot failure — roll back already-started modules.
                let rollback_err = rollback(&entries[..idx], &ctx).await;
                if let Err(rb_err) = rollback_err {
                    // Log rollback failure but return original start error.
                    eprintln!(
                        "[moduvex] rollback error after '{}' failed to start: {}",
                        entry.name, rb_err
                    );
                }
                return Err(ModuvexError::Lifecycle(
                    LifecycleError::new(e.to_string()).in_module(entry.name),
                ));
            }
        }

        hooks.notify_phase_entered(Phase::Start, &ctx).await?;
        hooks.notify_phase_entered(Phase::Ready, &ctx).await?;

        // ── Ready — wait for shutdown signal ─────────────────────────────────
        wait_for_shutdown(&shutdown_handle).await;

        // ── Shutdown sequence ────────────────────────────────────────────────
        hooks.notify_phase_entered(Phase::Stopping, &ctx).await?;

        // Stop modules in reverse boot order.
        let stop_err = rollback(&entries, &ctx).await;

        hooks.notify_phase_entered(Phase::Stopped, &ctx).await?;

        stop_err
    }
}

/// Call `on_stop()` on the given entries in reverse order.
///
/// Collects all errors and returns the first one (others are logged).
async fn rollback(
    entries: &[crate::module::registry::ModuleEntry],
    ctx: &AppContext,
) -> Result<()> {
    let mut first_err: Option<ModuvexError> = None;

    for entry in entries.iter().rev() {
        if let Err(e) = entry.lifecycle.on_stop(ctx).await {
            eprintln!("[moduvex] error stopping module '{}': {}", entry.name, e);
            if first_err.is_none() {
                first_err = Some(e);
            }
        }
    }

    match first_err {
        None => Ok(()),
        Some(e) => Err(e),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::registry::{ModuleEntry, ModuleRegistry};
    use crate::module::{Module, ModuleLifecycle};
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};

    // ── Test module that records start/stop calls ────────────────────────────

    struct RecordingModule {
        name: &'static str,
        log: Arc<Mutex<Vec<String>>>,
        fail_on_start: bool,
    }

    impl Module for RecordingModule {
        fn name(&self) -> &'static str {
            self.name
        }
    }

    impl ModuleLifecycle for RecordingModule {
        fn on_start<'a>(
            &'a self,
            _ctx: &'a AppContext,
        ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
            let log = Arc::clone(&self.log);
            let name = self.name;
            let fail = self.fail_on_start;
            Box::pin(async move {
                log.lock().unwrap().push(format!("start:{name}"));
                if fail {
                    Err(ModuvexError::Lifecycle(LifecycleError::new(
                        "forced failure",
                    )))
                } else {
                    Ok(())
                }
            })
        }

        fn on_stop<'a>(
            &'a self,
            _ctx: &'a AppContext,
        ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
            let log = Arc::clone(&self.log);
            let name = self.name;
            Box::pin(async move {
                log.lock().unwrap().push(format!("stop:{name}"));
                Ok(())
            })
        }
    }

    fn make_entry(name: &'static str, log: Arc<Mutex<Vec<String>>>, fail: bool) -> ModuleEntry {
        ModuleEntry {
            name,
            priority: 0,
            deps: vec![],
            lifecycle: Box::new(RecordingModule {
                name,
                log,
                fail_on_start: fail,
            }),
        }
    }

    #[test]
    fn clean_lifecycle_starts_and_stops_in_order() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut registry = ModuleRegistry::new();
        registry.push(make_entry("A", Arc::clone(&log), false));
        registry.push(make_entry("B", Arc::clone(&log), false));

        let ctx = Arc::new(AppContext::new());
        let engine = LifecycleEngine::new(registry, ctx);
        let handle = engine.shutdown_handle();

        moduvex_runtime::block_on(async move {
            // Trigger shutdown immediately so Ready → Stopping.
            handle.request();
            engine.run().await.unwrap();
        });

        let events = log.lock().unwrap().clone();
        // start A, start B, then stop B, stop A (reverse).
        assert_eq!(events, ["start:A", "start:B", "stop:B", "stop:A"]);
    }

    #[test]
    fn failed_start_triggers_rollback() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut registry = ModuleRegistry::new();
        registry.push(make_entry("A", Arc::clone(&log), false));
        registry.push(make_entry("B", Arc::clone(&log), true)); // B fails

        let ctx = Arc::new(AppContext::new());
        let engine = LifecycleEngine::new(registry, ctx);
        let handle = engine.shutdown_handle();
        handle.request();

        moduvex_runtime::block_on(async move {
            let result = engine.run().await;
            assert!(result.is_err(), "expected error from B's failed start");
        });

        let events = log.lock().unwrap().clone();
        // A started, B tried to start (logged before fail), then A rolled back.
        assert!(events.contains(&"start:A".to_string()));
        assert!(events.contains(&"start:B".to_string()));
        assert!(events.contains(&"stop:A".to_string()));
        // B never completed start so should not be stopped.
        assert!(!events.contains(&"stop:B".to_string()));
    }
}

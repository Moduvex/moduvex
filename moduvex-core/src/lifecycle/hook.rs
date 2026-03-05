//! `LifecycleHook` trait — framework-level hooks called at phase boundaries.
//!
//! Unlike `ModuleLifecycle` (which is per-module), `LifecycleHook` is for
//! framework-internal observers: metrics, tracing spans, health probes, etc.
//! Multiple hooks can be registered and are called in insertion order.

use std::future::Future;
use std::pin::Pin;

use crate::app::context::AppContext;
use crate::error::Result;
use crate::lifecycle::phase::Phase;

// ── LifecycleHook ─────────────────────────────────────────────────────────────

/// An observer that is notified at every phase transition.
///
/// Hooks are called *after* the transition completes successfully.
/// A hook returning `Err` aborts the transition and triggers rollback.
///
/// Object-safe: uses boxed futures (same approach as `ModuleLifecycle`).
pub trait LifecycleHook: Send + Sync + 'static {
    /// Called immediately after the application enters `phase`.
    fn on_phase_entered<'a>(
        &'a self,
        phase: Phase,
        ctx: &'a AppContext,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
}

// ── HookRegistry ──────────────────────────────────────────────────────────────

/// Registry of framework-level lifecycle hooks.
///
/// The `LifecycleEngine` calls each hook in insertion order after every
/// successful phase transition.
#[derive(Default)]
pub struct HookRegistry {
    hooks: Vec<Box<dyn LifecycleHook>>,
}

impl HookRegistry {
    /// Create an empty hook registry.
    pub fn new() -> Self {
        Self { hooks: Vec::new() }
    }

    /// Register a hook. Hooks are called in registration order.
    pub fn register(&mut self, hook: impl LifecycleHook) {
        self.hooks.push(Box::new(hook));
    }

    /// Notify all registered hooks that `phase` has been entered.
    ///
    /// Returns the first error encountered; remaining hooks are not called.
    pub async fn notify_phase_entered(&self, phase: Phase, ctx: &AppContext) -> Result<()> {
        for hook in &self.hooks {
            hook.on_phase_entered(phase, ctx).await?;
        }
        Ok(())
    }

    /// Number of registered hooks.
    pub fn len(&self) -> usize {
        self.hooks.len()
    }

    /// Whether no hooks are registered.
    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct TrackingHook {
        phases: Arc<Mutex<Vec<Phase>>>,
    }

    impl LifecycleHook for TrackingHook {
        fn on_phase_entered<'a>(
            &'a self,
            phase: Phase,
            _ctx: &'a AppContext,
        ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
            let phases = Arc::clone(&self.phases);
            Box::pin(async move {
                phases.lock().unwrap().push(phase);
                Ok(())
            })
        }
    }

    #[test]
    fn hook_receives_phase_notifications() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut registry = HookRegistry::new();
        registry.register(TrackingHook {
            phases: Arc::clone(&log),
        });

        let ctx = AppContext::new();
        moduvex_runtime::block_on(async {
            registry
                .notify_phase_entered(Phase::Config, &ctx)
                .await
                .unwrap();
            registry
                .notify_phase_entered(Phase::Init, &ctx)
                .await
                .unwrap();
        });

        let recorded = log.lock().unwrap().clone();
        assert_eq!(recorded, [Phase::Config, Phase::Init]);
    }

    #[test]
    fn empty_registry_is_noop() {
        let registry = HookRegistry::new();
        let ctx = AppContext::new();
        moduvex_runtime::block_on(async {
            registry
                .notify_phase_entered(Phase::Ready, &ctx)
                .await
                .unwrap();
        });
    }

    #[test]
    fn hook_error_aborts_remaining() {
        use crate::error::{classify::LifecycleError, ModuvexError};

        struct FailHook;
        impl LifecycleHook for FailHook {
            fn on_phase_entered<'a>(
                &'a self,
                _phase: Phase,
                _ctx: &'a AppContext,
            ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
                Box::pin(async { Err(ModuvexError::Lifecycle(LifecycleError::new("hook failed"))) })
            }
        }

        let called = Arc::new(Mutex::new(false));
        struct NeverCalled(Arc<Mutex<bool>>);
        impl LifecycleHook for NeverCalled {
            fn on_phase_entered<'a>(
                &'a self,
                _phase: Phase,
                _ctx: &'a AppContext,
            ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
                let flag = Arc::clone(&self.0);
                Box::pin(async move {
                    *flag.lock().unwrap() = true;
                    Ok(())
                })
            }
        }

        let mut registry = HookRegistry::new();
        registry.register(FailHook);
        registry.register(NeverCalled(Arc::clone(&called)));

        let ctx = AppContext::new();
        moduvex_runtime::block_on(async {
            let result = registry.notify_phase_entered(Phase::Start, &ctx).await;
            assert!(result.is_err());
        });

        assert!(
            !*called.lock().unwrap(),
            "second hook should not have been called"
        );
    }

    #[test]
    fn hook_registry_len() {
        let mut registry = HookRegistry::new();
        assert_eq!(registry.len(), 0);
        assert!(registry.is_empty());
        registry.register(TrackingHook {
            phases: Arc::new(Mutex::new(Vec::new())),
        });
        assert_eq!(registry.len(), 1);
        assert!(!registry.is_empty());
    }

    #[test]
    fn multiple_hooks_all_called_in_order() {
        let log = Arc::new(Mutex::new(Vec::<usize>::new()));

        struct IndexHook {
            idx: usize,
            log: Arc<Mutex<Vec<usize>>>,
        }

        impl LifecycleHook for IndexHook {
            fn on_phase_entered<'a>(
                &'a self,
                _phase: Phase,
                _ctx: &'a AppContext,
            ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
                let log = Arc::clone(&self.log);
                let idx = self.idx;
                Box::pin(async move {
                    log.lock().unwrap().push(idx);
                    Ok(())
                })
            }
        }

        let mut registry = HookRegistry::new();
        for i in 0..3 {
            registry.register(IndexHook { idx: i, log: Arc::clone(&log) });
        }

        let ctx = AppContext::new();
        moduvex_runtime::block_on(async {
            registry.notify_phase_entered(Phase::Start, &ctx).await.unwrap();
        });

        let recorded = log.lock().unwrap().clone();
        assert_eq!(recorded, vec![0, 1, 2]);
    }

    #[test]
    fn hook_receives_all_phases_in_sequence() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut registry = HookRegistry::new();
        registry.register(TrackingHook { phases: Arc::clone(&log) });

        let ctx = AppContext::new();
        let all_phases = [
            Phase::Config,
            Phase::Validate,
            Phase::Init,
            Phase::Start,
            Phase::Ready,
            Phase::Stopping,
            Phase::Stopped,
        ];

        moduvex_runtime::block_on(async {
            for phase in all_phases {
                registry.notify_phase_entered(phase, &ctx).await.unwrap();
            }
        });

        let recorded = log.lock().unwrap().clone();
        assert_eq!(recorded, all_phases.to_vec());
    }

    #[test]
    fn default_hook_registry_is_empty() {
        let registry = HookRegistry::default();
        assert!(registry.is_empty());
    }
}

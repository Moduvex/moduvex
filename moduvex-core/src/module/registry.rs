//! Runtime module registry — holds trait-object references for lifecycle calls.
//!
//! The type-state system handles compile-time dependency checking. This registry
//! provides the runtime backing: an ordered `Vec` of boxed `ModuleLifecycle`
//! trait objects that the `LifecycleEngine` iterates through.
//!
//! Modules are stored in topological boot order (dependency-first). Shutdown
//! iterates in reverse.

use crate::module::ModuleLifecycle;

// ── ModuleEntry ───────────────────────────────────────────────────────────────

/// A registered module plus its metadata.
pub struct ModuleEntry {
    /// The module's human-readable name (for logging/errors).
    pub name: &'static str,
    /// Startup priority — higher means earlier within same dependency tier.
    pub priority: i32,
    /// The boxed trait object used for lifecycle calls.
    pub lifecycle: Box<dyn ModuleLifecycle>,
}

// ── ModuleRegistry ────────────────────────────────────────────────────────────

/// Runtime registry of modules, ordered for deterministic lifecycle execution.
///
/// After construction via the type-state builder, modules are sorted by
/// (dependency order, then priority descending) before the `LifecycleEngine`
/// takes ownership.
pub struct ModuleRegistry {
    entries: Vec<ModuleEntry>,
}

impl ModuleRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self { entries: Vec::new() }
    }

    /// Append a module entry.
    ///
    /// Entries should be added in the order they will be started (dependency-
    /// first). The builder appends in reverse registration order, so the
    /// registry sorts before handing to the engine.
    pub fn push(&mut self, entry: ModuleEntry) {
        self.entries.push(entry);
    }

    /// Sort entries by priority descending within the existing insertion order.
    ///
    /// This is stable — modules at the same priority keep their insertion order.
    pub fn sort_by_priority(&mut self) {
        self.entries.sort_by(|a, b| b.priority.cmp(&a.priority));
    }

    /// Number of registered modules.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate entries in boot order (forward).
    pub fn iter(&self) -> impl Iterator<Item = &ModuleEntry> {
        self.entries.iter()
    }

    /// Iterate entries in shutdown order (reverse of boot order).
    pub fn iter_rev(&self) -> impl Iterator<Item = &ModuleEntry> {
        self.entries.iter().rev()
    }

    /// Consume the registry and return the ordered entries.
    pub fn into_entries(self) -> Vec<ModuleEntry> {
        self.entries
    }
}

impl Default for ModuleRegistry {
    fn default() -> Self { Self::new() }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;
    use std::future::Future;
    use crate::app::context::AppContext;
    use crate::error::Result;
    use crate::module::Module;

    struct FakeMod { name: &'static str, prio: i32 }

    impl Module for FakeMod {
        fn name(&self) -> &'static str { self.name }
        fn priority(&self) -> i32 { self.prio }
    }

    impl ModuleLifecycle for FakeMod {
        fn on_start<'a>(&'a self, _ctx: &'a AppContext)
            -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>
        {
            Box::pin(async { Ok(()) })
        }
        fn on_stop<'a>(&'a self, _ctx: &'a AppContext)
            -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>
        {
            Box::pin(async { Ok(()) })
        }
    }

    fn entry(name: &'static str, prio: i32) -> ModuleEntry {
        ModuleEntry { name, priority: prio, lifecycle: Box::new(FakeMod { name, prio }) }
    }

    #[test]
    fn push_and_len() {
        let mut reg = ModuleRegistry::new();
        reg.push(entry("a", 0));
        reg.push(entry("b", 0));
        assert_eq!(reg.len(), 2);
    }

    #[test]
    fn sort_by_priority_orders_highest_first() {
        let mut reg = ModuleRegistry::new();
        reg.push(entry("low", 0));
        reg.push(entry("high", 100));
        reg.push(entry("mid", 50));
        reg.sort_by_priority();
        let names: Vec<_> = reg.iter().map(|e| e.name).collect();
        assert_eq!(names, ["high", "mid", "low"]);
    }

    #[test]
    fn iter_rev_is_reverse_of_iter() {
        let mut reg = ModuleRegistry::new();
        reg.push(entry("first", 0));
        reg.push(entry("second", 0));
        let fwd: Vec<_> = reg.iter().map(|e| e.name).collect();
        let rev: Vec<_> = reg.iter_rev().map(|e| e.name).collect();
        assert_eq!(fwd, ["first", "second"]);
        assert_eq!(rev, ["second", "first"]);
    }
}

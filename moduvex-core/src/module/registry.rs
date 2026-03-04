//! Runtime module registry — holds trait-object references for lifecycle calls.
//!
//! The type-state system handles compile-time dependency checking. This registry
//! provides the runtime backing: an ordered `Vec` of boxed `ModuleLifecycle`
//! trait objects that the `LifecycleEngine` iterates through.
//!
//! Modules are stored in topological boot order (dependency-first). Shutdown
//! iterates in reverse.

use crate::error::{ModuvexError, Result};
use crate::module::ModuleLifecycle;

// ── ModuleEntry ───────────────────────────────────────────────────────────────

/// A registered module plus its metadata.
pub struct ModuleEntry {
    /// The module's human-readable name (for logging/errors).
    pub name: &'static str,
    /// Startup priority — higher means earlier within same dependency tier.
    pub priority: i32,
    /// Declared dependency names (module names this module requires).
    pub deps: Vec<&'static str>,
    /// The boxed trait object used for lifecycle calls.
    pub lifecycle: Box<dyn ModuleLifecycle>,
}

// ── ModuleRegistry ────────────────────────────────────────────────────────────

/// Runtime registry of modules, ordered for deterministic lifecycle execution.
///
/// After construction via the type-state builder, modules are sorted by
/// topological dependency order (dependency-first), then by priority descending
/// within the same depth tier. Call `topological_sort()` before handing to
/// the `LifecycleEngine`.
pub struct ModuleRegistry {
    entries: Vec<ModuleEntry>,
}

impl ModuleRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
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

    /// Sort entries topologically based on declared dependencies.
    ///
    /// Uses Kahn's algorithm (BFS-based topological sort). Within the same
    /// dependency tier (same depth), modules are ordered by priority descending.
    ///
    /// Returns `Err` if a circular dependency is detected.
    pub fn topological_sort(&mut self) -> Result<()> {
        let n = self.entries.len();
        if n == 0 {
            return Ok(());
        }

        // Build index map: name → index in entries
        let mut name_to_idx: std::collections::HashMap<&'static str, usize> =
            std::collections::HashMap::new();
        for (i, entry) in self.entries.iter().enumerate() {
            name_to_idx.insert(entry.name, i);
        }

        // Build adjacency: for each module, list indices that depend on it
        // (i.e., edges go from dependency → dependent).
        // Also compute in-degree (number of unresolved deps) for each module.
        let mut in_degree = vec![0usize; n];
        // dependents[i] = list of module indices that have i as a dependency
        let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];

        for (i, entry) in self.entries.iter().enumerate() {
            for &dep_name in &entry.deps {
                if let Some(&dep_idx) = name_to_idx.get(dep_name) {
                    dependents[dep_idx].push(i);
                    in_degree[i] += 1;
                }
                // Unknown dep names are ignored at runtime (compile-time
                // guarantees they exist via the type-state builder).
            }
        }

        // Kahn's algorithm: start with all modules that have no pending deps.
        // Use a max-heap keyed by (priority, index) so within the same dependency
        // tier, the highest-priority module is processed first.
        let mut ready: std::collections::BinaryHeap<(i32, usize)> =
            std::collections::BinaryHeap::new();

        for (i, &deg) in in_degree.iter().enumerate() {
            if deg == 0 {
                ready.push((self.entries[i].priority, i));
            }
        }

        let mut sorted_indices: Vec<usize> = Vec::with_capacity(n);

        while let Some((_, idx)) = ready.pop() {
            sorted_indices.push(idx);
            for &dep_of in &dependents[idx] {
                in_degree[dep_of] -= 1;
                if in_degree[dep_of] == 0 {
                    ready.push((self.entries[dep_of].priority, dep_of));
                }
            }
        }

        if sorted_indices.len() != n {
            // Not all modules were processed — circular dependency exists.
            let remaining: Vec<&str> = (0..n)
                .filter(|i| !sorted_indices.contains(i))
                .map(|i| self.entries[i].name)
                .collect();
            return Err(ModuvexError::Lifecycle(
                crate::error::LifecycleError::new(format!(
                    "circular dependency detected among modules: {}",
                    remaining.join(", ")
                )),
            ));
        }

        // Reorder entries according to sorted_indices.
        // We drain the original Vec and reconstruct in sorted order.
        // Use a temporary Vec of Option to allow indexed moves.
        let mut temp: Vec<Option<ModuleEntry>> = self
            .entries
            .drain(..)
            .map(Some)
            .collect();

        for idx in sorted_indices {
            self.entries.push(temp[idx].take().unwrap());
        }

        Ok(())
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
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::context::AppContext;
    use crate::error::Result;
    use crate::module::Module;
    use std::future::Future;
    use std::pin::Pin;

    struct FakeMod {
        name: &'static str,
        prio: i32,
    }

    impl Module for FakeMod {
        fn name(&self) -> &'static str {
            self.name
        }
        fn priority(&self) -> i32 {
            self.prio
        }
    }

    impl ModuleLifecycle for FakeMod {
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

    fn entry(name: &'static str, prio: i32) -> ModuleEntry {
        ModuleEntry {
            name,
            priority: prio,
            deps: vec![],
            lifecycle: Box::new(FakeMod { name, prio }),
        }
    }

    fn entry_with_deps(name: &'static str, prio: i32, deps: Vec<&'static str>) -> ModuleEntry {
        ModuleEntry {
            name,
            priority: prio,
            deps,
            lifecycle: Box::new(FakeMod { name, prio }),
        }
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

    #[test]
    fn topological_sort_no_deps() {
        let mut reg = ModuleRegistry::new();
        reg.push(entry("low", 0));
        reg.push(entry("high", 100));
        reg.push(entry("mid", 50));
        reg.topological_sort().unwrap();
        // No deps — should be sorted by priority descending.
        let names: Vec<_> = reg.iter().map(|e| e.name).collect();
        assert_eq!(names, ["high", "mid", "low"]);
    }

    #[test]
    fn topological_sort_respects_dependencies() {
        let mut reg = ModuleRegistry::new();
        // "db" has no deps; "user" depends on "db"; "api" depends on "user".
        // Push in wrong priority order to verify topo sort overrides priority.
        reg.push(entry_with_deps("api", 100, vec!["user"]));
        reg.push(entry_with_deps("user", 50, vec!["db"]));
        reg.push(entry("db", 0));
        reg.topological_sort().unwrap();
        let names: Vec<_> = reg.iter().map(|e| e.name).collect();
        // "db" must come before "user", "user" before "api".
        let db_pos = names.iter().position(|&n| n == "db").unwrap();
        let user_pos = names.iter().position(|&n| n == "user").unwrap();
        let api_pos = names.iter().position(|&n| n == "api").unwrap();
        assert!(db_pos < user_pos, "db must start before user");
        assert!(user_pos < api_pos, "user must start before api");
    }

    #[test]
    fn topological_sort_same_tier_by_priority() {
        let mut reg = ModuleRegistry::new();
        // "a" and "b" both depend on "root"; within that tier, priority wins.
        reg.push(entry_with_deps("b", 10, vec!["root"]));
        reg.push(entry_with_deps("a", 50, vec!["root"]));
        reg.push(entry("root", 0));
        reg.topological_sort().unwrap();
        let names: Vec<_> = reg.iter().map(|e| e.name).collect();
        assert_eq!(names[0], "root");
        // "a" has higher priority → comes before "b" in same tier.
        let a_pos = names.iter().position(|&n| n == "a").unwrap();
        let b_pos = names.iter().position(|&n| n == "b").unwrap();
        assert!(a_pos < b_pos, "higher priority module 'a' should come before 'b'");
    }

    #[test]
    fn topological_sort_detects_cycle() {
        let mut reg = ModuleRegistry::new();
        // a → b → a (cycle)
        reg.push(entry_with_deps("a", 0, vec!["b"]));
        reg.push(entry_with_deps("b", 0, vec!["a"]));
        let result = reg.topological_sort();
        assert!(result.is_err(), "should detect circular dependency");
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("circular"), "error should mention circular: {msg}");
    }

    #[test]
    fn topological_sort_empty_registry() {
        let mut reg = ModuleRegistry::new();
        reg.topological_sort().unwrap();
        assert_eq!(reg.len(), 0);
    }
}

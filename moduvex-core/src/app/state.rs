//! State markers for the type-state builder pattern.
//!
//! These zero-sized types encode the builder state in the type system so that
//! method availability is enforced at compile time — no runtime checks needed.

// ── State markers ─────────────────────────────────────────────────────────────

/// Initial state: no configuration has been provided.
///
/// Only `Moduvex::new()` produces this state; only `.config()` is callable.
pub struct Unconfigured;

/// Configuration has been loaded.
///
/// `.module::<M>()` and `.run()` are available in this state.
pub struct Configured;

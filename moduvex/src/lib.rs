//! # Moduvex
//!
//! **Structure before scale.** A structured backend framework for Rust with
//! a custom async runtime, zero 3rd-party async dependencies.
//!
//! This umbrella crate re-exports all Moduvex sub-crates for convenience.
//! Use feature flags to include only what you need.
//!
//! ```rust,ignore
//! use moduvex::prelude::*;
//!
//! #[moduvex::main]
//! async fn main() {
//!     Moduvex::new().run().await;
//! }
//! ```

// ── Always available ──

pub use moduvex_runtime as runtime;
pub use moduvex_core as core;
pub use moduvex_config as config;

// ── Feature-gated re-exports ──

#[cfg(feature = "web")]
pub use moduvex_http as http;

#[cfg(feature = "web")]
pub use moduvex_observe as observe;

#[cfg(feature = "web")]
pub use moduvex_starter_web as starter_web;

#[cfg(feature = "data")]
pub use moduvex_db as db;

#[cfg(feature = "data")]
pub use moduvex_starter_data as starter_data;

// ── Prelude ──

pub mod prelude {
    // Core (always available)
    pub use moduvex_core::prelude::*;
    pub use moduvex_config::{ConfigLoader, Profile};

    // Web feature
    #[cfg(feature = "web")]
    pub use moduvex_http::{HttpServer, Request, Response, Router, StatusCode};

    #[cfg(feature = "web")]
    pub use moduvex_observe::{info, warn, error, debug, trace_event};

    #[cfg(feature = "web")]
    pub use moduvex_observe::{Counter, Gauge, Histogram, Span};

    // Data feature
    #[cfg(feature = "data")]
    pub use moduvex_db::{ConnectionPool, PoolConfig, Row, RowSet, Transaction};
}

#[cfg(test)]
mod tests {
    #[test]
    fn prelude_imports_core() {
        // Verifies the prelude compiles with default features.
        use crate::prelude::*;
        let _ = std::any::type_name::<ConfigLoader>();
    }
}

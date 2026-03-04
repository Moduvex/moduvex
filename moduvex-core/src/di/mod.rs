//! DI subsystem barrel — re-exports the public DI surface.
//!
//! The three layers of the DI system:
//! - `scope`    — `TypeMap`, the startup-only singleton store
//! - `provider` — `Provider` trait, `Singleton<T>`, `RequestScoped<T>`, `Inject`

pub mod provider;
pub mod scope;

pub use provider::{Inject, Provider, RequestScoped, Singleton};
pub use scope::TypeMap;

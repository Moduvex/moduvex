//! Built-in middleware: CORS, Timeout.

pub mod cors;
pub mod timeout;

pub use cors::Cors;
pub use timeout::Timeout;

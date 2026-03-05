//! Built-in middleware: CORS, Timeout, StaticFiles, RequestId.

pub mod cors;
pub mod request_id;
pub mod static_files;
pub mod timeout;

pub use cors::Cors;
pub use request_id::RequestId;
pub use request_id::RequestIdValue;
pub use static_files::StaticFiles;
pub use timeout::Timeout;

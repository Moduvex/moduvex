//! HTTP routing — method enum, path matching, radix-tree router.

pub mod method;
pub mod path;
pub mod router;

pub use method::Method;
pub use path::{match_path, parse_pattern, PathSegment};
pub use router::{BoxHandler, RouteMatch, Router};

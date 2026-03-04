//! HTTP routing — method enum, path matching, radix-tree router.

pub mod method;
pub mod path;
pub mod router;

pub use method::Method;
pub use path::{PathSegment, match_path, parse_pattern};
pub use router::{BoxHandler, Router, RouteMatch};

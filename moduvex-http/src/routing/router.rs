//! Type-safe HTTP router — method + path matching with radix-tree lookup.
//!
//! Uses a per-method radix tree for O(path_length) dispatch instead of a
//! linear scan. Supports `:param` single-segment capture and `*wildcard`
//! multi-segment capture. Priority: literal > param > wildcard.
//!
//! # Usage
//! ```ignore
//! let router = Router::new()
//!     .get("/users", list_users)
//!     .get("/users/:id", get_user)
//!     .post("/users", create_user);
//! ```

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::extract::IntoHandler;
use crate::request::Request;
use crate::response::Response;
use crate::routing::method::Method;
use crate::routing::path::parse_pattern;
use crate::routing::radix::Node;

// ── Handler type-erasure ───────────────────────────────────────────────────────

/// A boxed async handler: `Request → Response`.
///
/// Type-erased so heterogeneous handler functions can be stored in the router.
pub type BoxHandler = Box<
    dyn Fn(Request) -> Pin<Box<dyn Future<Output = Response> + Send + 'static>>
        + Send
        + Sync
        + 'static,
>;

/// Wrap any `async fn(Request) -> Response` (or equivalent) into a `BoxHandler`.
pub fn into_box_handler<F, Fut>(f: F) -> BoxHandler
where
    F: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    Box::new(move |req| Box::pin(f(req)))
}

// ── Match result ──────────────────────────────────────────────────────────────

/// Result of a successful router lookup.
pub struct RouteMatch<'r> {
    /// Reference to the matched handler (Arc-wrapped for middleware compatibility).
    pub handler: &'r Arc<BoxHandler>,
    /// Extracted path params as `(name, value)` pairs.
    pub params: Vec<(String, String)>,
}

// ── Router ────────────────────────────────────────────────────────────────────

/// HTTP router: maps (method, path) → handler via per-method radix trees.
///
/// Each HTTP method has its own radix tree so lookups never need to filter by
/// method — the tree is selected first, then the path is resolved in O(depth).
pub struct Router {
    /// One radix tree per HTTP method.
    trees: HashMap<Method, Node>,
    /// Optional catch-all handler for unmatched requests.
    fallback: Option<Arc<BoxHandler>>,
}

impl Router {
    /// Create an empty router.
    pub fn new() -> Self {
        Self {
            trees: HashMap::new(),
            fallback: None,
        }
    }

    // ── Route registration ─────────────────────────────────────────────────

    /// Register a route for an arbitrary method.
    pub fn route<T>(mut self, method: Method, pattern: &str, handler: impl IntoHandler<T>) -> Self {
        let segments = parse_pattern(pattern);
        let handler = Arc::new(handler.into_box_handler());
        self.trees
            .entry(method)
            .or_insert_with(Node::new_root)
            .insert(&segments, handler);
        self
    }

    pub fn get<T>(self, pattern: &str, h: impl IntoHandler<T>) -> Self {
        self.route(Method::GET, pattern, h)
    }

    pub fn post<T>(self, pattern: &str, h: impl IntoHandler<T>) -> Self {
        self.route(Method::POST, pattern, h)
    }

    pub fn put<T>(self, pattern: &str, h: impl IntoHandler<T>) -> Self {
        self.route(Method::PUT, pattern, h)
    }

    pub fn delete<T>(self, pattern: &str, h: impl IntoHandler<T>) -> Self {
        self.route(Method::DELETE, pattern, h)
    }

    pub fn patch<T>(self, pattern: &str, h: impl IntoHandler<T>) -> Self {
        self.route(Method::PATCH, pattern, h)
    }

    pub fn options<T>(self, pattern: &str, h: impl IntoHandler<T>) -> Self {
        self.route(Method::OPTIONS, pattern, h)
    }

    /// Set a fallback handler for unmatched requests (returns 404 by default).
    pub fn fallback<T>(mut self, handler: impl IntoHandler<T>) -> Self {
        self.fallback = Some(Arc::new(handler.into_box_handler()));
        self
    }

    /// Mount a sub-router under `prefix`. All sub-routes are re-inserted with
    /// the prefix prepended. This flattens the mounted router at insertion time
    /// so the radix tree sees complete paths — no special "mount" logic needed
    /// at lookup time.
    pub fn nest(mut self, prefix: &str, other: Router) -> Self {
        let prefix = prefix.trim_end_matches('/');
        // Walk the other router's trees and re-insert every route into self.
        for (method, root) in other.trees {
            let self_root = self.trees.entry(method).or_insert_with(Node::new_root);
            // Flatten: collect all (segments, handler) pairs from the sub-tree.
            let mut routes: Vec<(Vec<crate::routing::path::PathSegment>, Arc<BoxHandler>)> =
                Vec::new();
            collect_routes(&root, &mut Vec::new(), &mut routes);
            let prefix_segs = parse_pattern(prefix);
            for (sub_segs, handler) in routes {
                let mut full_segs = prefix_segs.clone();
                full_segs.extend(sub_segs);
                self_root.insert(&full_segs, handler);
            }
        }
        self
    }

    // ── Dispatch ───────────────────────────────────────────────────────────

    /// Find the best matching route for `(method, path)`.
    ///
    /// HEAD requests automatically fall back to GET routes per RFC 9110 §9.3.2.
    pub fn lookup<'r>(&'r self, method: Method, path: &str) -> Option<RouteMatch<'r>> {
        if let Some(m) = self.lookup_method(method, path) {
            return Some(m);
        }
        if method == Method::HEAD {
            return self.lookup_method(Method::GET, path);
        }
        None
    }

    /// Get the fallback handler (if any).
    pub fn fallback_handler(&self) -> Option<&Arc<BoxHandler>> {
        self.fallback.as_ref()
    }

    // ── Internal ───────────────────────────────────────────────────────────

    fn lookup_method<'r>(&'r self, method: Method, path: &str) -> Option<RouteMatch<'r>> {
        let tree = self.trees.get(&method)?;
        let parts = split_path(path);
        let mut params = Vec::new();
        let handler = tree.lookup(&parts, &mut params)?;
        Some(RouteMatch { handler, params })
    }
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Split a URL path into non-empty segments, stripping the leading slash.
fn split_path(path: &str) -> Vec<&str> {
    let p = path.trim_start_matches('/');
    if p.is_empty() {
        vec![]
    } else {
        p.split('/').collect()
    }
}

/// Recursively collect all (full_segment_path, handler) pairs from a subtree.
///
/// Used by `nest()` to flatten a mounted router's routes before re-insertion.
fn collect_routes(
    node: &Node,
    current: &mut Vec<crate::routing::path::PathSegment>,
    out: &mut Vec<(Vec<crate::routing::path::PathSegment>, Arc<BoxHandler>)>,
) {
    if let Some(h) = &node.handler {
        out.push((current.clone(), h.clone()));
    }
    node.visit_children(|child_seg, child_node| {
        current.push(child_seg);
        collect_routes(child_node, current, out);
        current.pop();
    });
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::response::Response;
    use crate::status::StatusCode;

    async fn ok_handler(_req: Request) -> Response {
        Response::new(StatusCode::OK)
    }

    #[test]
    fn static_route_match() {
        let router = Router::new().get("/hello", ok_handler);
        assert!(router.lookup(Method::GET, "/hello").is_some());
        assert!(router.lookup(Method::GET, "/world").is_none());
    }

    #[test]
    fn param_route_match() {
        let router = Router::new().get("/users/:id", ok_handler);
        let m = router.lookup(Method::GET, "/users/42").unwrap();
        assert_eq!(m.params, vec![("id".to_string(), "42".to_string())]);
    }

    #[test]
    fn head_falls_back_to_get() {
        let router = Router::new().get("/ping", ok_handler);
        assert!(router.lookup(Method::HEAD, "/ping").is_some());
    }

    #[test]
    fn method_mismatch() {
        let router = Router::new().get("/data", ok_handler);
        assert!(router.lookup(Method::POST, "/data").is_none());
    }

    #[test]
    fn nested_router() {
        async fn handler(_req: Request) -> Response {
            Response::new(StatusCode::OK)
        }
        let api = Router::new().get("/users", handler);
        let root = Router::new().nest("/api/v1", api);
        assert!(root.lookup(Method::GET, "/api/v1/users").is_some());
    }

    #[test]
    fn wildcard_route_captures_remainder() {
        let router = Router::new().get("/files/*path", ok_handler);
        let m = router.lookup(Method::GET, "/files/a/b/c").unwrap();
        assert_eq!(
            m.params,
            vec![("path".to_string(), "a/b/c".to_string())]
        );
    }

    #[test]
    fn multiple_params_in_route() {
        let router = Router::new().get("/users/:uid/posts/:pid", ok_handler);
        let m = router.lookup(Method::GET, "/users/42/posts/7").unwrap();
        assert!(m.params.contains(&("uid".to_string(), "42".to_string())));
        assert!(m.params.contains(&("pid".to_string(), "7".to_string())));
    }

    #[test]
    fn fallback_handler_is_accessible() {
        let router = Router::new()
            .get("/hello", ok_handler)
            .fallback(ok_handler);
        assert!(router.fallback_handler().is_some());
    }

    #[test]
    fn no_fallback_returns_none() {
        let router = Router::new().get("/only", ok_handler);
        assert!(router.fallback_handler().is_none());
    }

    #[test]
    fn put_method_registered_and_matched() {
        let router = Router::new().put("/resource/:id", ok_handler);
        let m = router.lookup(Method::PUT, "/resource/5").unwrap();
        assert_eq!(m.params[0], ("id".to_string(), "5".to_string()));
    }

    #[test]
    fn delete_method_registered_and_matched() {
        let router = Router::new().delete("/items/:id", ok_handler);
        assert!(router.lookup(Method::DELETE, "/items/3").is_some());
    }

    #[test]
    fn patch_method_registered_and_matched() {
        let router = Router::new().patch("/things/:id", ok_handler);
        assert!(router.lookup(Method::PATCH, "/things/9").is_some());
    }

    #[test]
    fn options_method_registered_and_matched() {
        let router = Router::new().options("/resource", ok_handler);
        assert!(router.lookup(Method::OPTIONS, "/resource").is_some());
    }

    #[test]
    fn nested_router_with_params() {
        let inner = Router::new().get("/users/:id", ok_handler);
        let root = Router::new().nest("/api", inner);
        let m = root.lookup(Method::GET, "/api/users/42").unwrap();
        assert_eq!(m.params[0], ("id".to_string(), "42".to_string()));
    }

    #[test]
    fn nested_deep_prefix() {
        let inner = Router::new().get("/items", ok_handler);
        let root = Router::new().nest("/api/v2", inner);
        assert!(root.lookup(Method::GET, "/api/v2/items").is_some());
        assert!(root.lookup(Method::GET, "/api/items").is_none());
    }

    #[test]
    fn head_without_get_route_returns_none() {
        let router = Router::new().post("/data", ok_handler);
        assert!(router.lookup(Method::HEAD, "/data").is_none());
    }

    #[test]
    fn post_and_get_same_path_different_methods() {
        let router = Router::new()
            .get("/resource", ok_handler)
            .post("/resource", ok_handler);
        assert!(router.lookup(Method::GET, "/resource").is_some());
        assert!(router.lookup(Method::POST, "/resource").is_some());
        assert!(router.lookup(Method::DELETE, "/resource").is_none());
    }

    #[test]
    fn root_path_route() {
        let router = Router::new().get("/", ok_handler);
        assert!(router.lookup(Method::GET, "/").is_some());
        assert!(router.lookup(Method::GET, "/extra").is_none());
    }

    #[test]
    fn router_default_is_empty() {
        let router = Router::default();
        assert!(router.lookup(Method::GET, "/").is_none());
        assert!(router.fallback_handler().is_none());
    }

    #[test]
    fn route_all_methods_registered() {
        let router = Router::new()
            .get("/r", ok_handler)
            .post("/r", ok_handler)
            .put("/r", ok_handler)
            .delete("/r", ok_handler)
            .patch("/r", ok_handler)
            .options("/r", ok_handler);
        for method in [
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::PATCH,
            Method::OPTIONS,
        ] {
            assert!(
                router.lookup(method, "/r").is_some(),
                "method {method:?} should match"
            );
        }
    }
}

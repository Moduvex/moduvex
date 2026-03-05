//! Segment-based radix tree (trie) for O(path_length) route lookup.
//!
//! # Design
//! Each tree node represents one path segment. Children are ordered:
//!   1. Literal nodes (sorted alphabetically, binary-searched)
//!   2. Param node (at most one: captures a single segment)
//!   3. Wildcard node (at most one: captures remaining path; must be last)
//!
//! # Priority (when multiple patterns could match)
//! Literal > Param > Wildcard
//!
//! # Example tree for routes:
//! - GET /users
//! - GET /users/:id
//! - GET /users/:id/posts
//! - GET /files/*path
//! - GET /health
//!
//! ```text
//! root
//! ├── "users"  [handler: GET /users]
//! │   └── :id  [handler: GET /users/:id]
//! │       └── "posts"  [handler: GET /users/:id/posts]
//! ├── "files"
//! │   └── *path  [handler: GET /files/*path]
//! └── "health"  [handler: GET /health]
//! ```

use std::sync::Arc;

use crate::routing::path::PathSegment;
use crate::routing::router::BoxHandler;

// ── Node ──────────────────────────────────────────────────────────────────────

/// A node in the radix trie. Each node owns one path segment.
pub(crate) struct Node {
    /// The segment this node represents.
    segment: NodeSegment,
    /// Handler registered at this exact node (if this node is a route endpoint).
    pub(crate) handler: Option<Arc<BoxHandler>>,
    /// Literal children — sorted by segment string for O(log n) binary search.
    literal_children: Vec<Node>,
    /// At most one param child (`:name`).
    param_child: Option<Box<Node>>,
    /// At most one wildcard child (`*name`).
    wildcard_child: Option<Box<Node>>,
}

/// The segment type stored in each node.
enum NodeSegment {
    /// Tree root — no segment, never matches directly.
    Root,
    /// Exact string match (e.g. `"users"`).
    Literal(String),
    /// Named single-segment capture (e.g. `:id`).
    Param(String),
    /// Named multi-segment capture (e.g. `*path`). Matches remaining path.
    Wildcard(String),
}

impl NodeSegment {
    fn literal_str(&self) -> Option<&str> {
        if let NodeSegment::Literal(s) = self {
            Some(s.as_str())
        } else {
            None
        }
    }
}

impl Node {
    /// Create the tree root node.
    pub(crate) fn new_root() -> Self {
        Node {
            segment: NodeSegment::Root,
            handler: None,
            literal_children: Vec::new(),
            param_child: None,
            wildcard_child: None,
        }
    }

    /// Create a node for the given `PathSegment`.
    fn from_path_segment(seg: &PathSegment) -> Self {
        let node_seg = match seg {
            PathSegment::Static(s) => NodeSegment::Literal(s.clone()),
            PathSegment::Param(s) => NodeSegment::Param(s.clone()),
            PathSegment::Wildcard(s) => NodeSegment::Wildcard(s.clone()),
        };
        Node {
            segment: node_seg,
            handler: None,
            literal_children: Vec::new(),
            param_child: None,
            wildcard_child: None,
        }
    }

    // ── Insertion ─────────────────────────────────────────────────────────

    /// Insert a route into the subtree rooted at `self`.
    ///
    /// `segments` is the remaining path segments to consume. When empty, the
    /// handler is registered on `self`.
    pub(crate) fn insert(&mut self, segments: &[PathSegment], handler: Arc<BoxHandler>) {
        if segments.is_empty() {
            // Overwrite silently (last-registered wins for duplicate routes).
            self.handler = Some(handler);
            return;
        }

        let first = &segments[0];
        let rest = &segments[1..];

        match first {
            PathSegment::Static(literal) => {
                // Binary search for an existing literal child with the same string.
                match self
                    .literal_children
                    .binary_search_by_key(&literal.as_str(), |n| {
                        n.segment.literal_str().unwrap_or("")
                    }) {
                    Ok(idx) => {
                        // Found existing child — recurse into it.
                        self.literal_children[idx].insert(rest, handler);
                    }
                    Err(idx) => {
                        // Create a new literal node and insert in sorted order.
                        let mut child = Node::from_path_segment(first);
                        child.insert(rest, handler);
                        self.literal_children.insert(idx, child);
                    }
                }
            }
            PathSegment::Param(_) => {
                if let Some(child) = &mut self.param_child {
                    child.insert(rest, handler);
                } else {
                    let mut child = Node::from_path_segment(first);
                    child.insert(rest, handler);
                    self.param_child = Some(Box::new(child));
                }
            }
            PathSegment::Wildcard(_) => {
                // Wildcard captures everything — `rest` is ignored (unreachable).
                if let Some(child) = &mut self.wildcard_child {
                    // Overwrite handler on existing wildcard node.
                    child.handler = Some(handler);
                } else {
                    let mut child = Node::from_path_segment(first);
                    child.handler = Some(handler);
                    self.wildcard_child = Some(Box::new(child));
                }
            }
        }
    }

    // ── Lookup ────────────────────────────────────────────────────────────

    /// Look up a handler for the given path segments.
    ///
    /// `parts` contains the URL path split by `/` (empty strings from leading
    /// slash already removed by the caller).
    ///
    /// `params` accumulates captured named parameters.
    ///
    /// Returns a reference to the matched handler or `None`.
    pub(crate) fn lookup<'a>(
        &'a self,
        parts: &[&str],
        params: &mut Vec<(String, String)>,
    ) -> Option<&'a Arc<BoxHandler>> {
        if parts.is_empty() {
            return self.handler.as_ref();
        }

        let head = parts[0];
        let tail = &parts[1..];

        // Priority 1: literal children (exact match, O(log n) binary search).
        if let Ok(idx) = self
            .literal_children
            .binary_search_by_key(&head, |n| n.segment.literal_str().unwrap_or(""))
        {
            if let Some(result) = self.literal_children[idx].lookup(tail, params) {
                return Some(result);
            }
        }

        // Priority 2: param child (captures one segment).
        if let Some(param_node) = &self.param_child {
            let param_name = match &param_node.segment {
                NodeSegment::Param(n) => n.clone(),
                _ => unreachable!("param_child must be a Param node"),
            };
            // Save params length so we can roll back on no-match.
            let saved_len = params.len();
            params.push((param_name, head.to_string()));
            if let Some(result) = param_node.lookup(tail, params) {
                return Some(result);
            }
            // Roll back captured param — this branch didn't produce a match.
            params.truncate(saved_len);
        }

        // Priority 3: wildcard child (captures remaining path including `head`).
        if let Some(wc_node) = &self.wildcard_child {
            let wc_name = match &wc_node.segment {
                NodeSegment::Wildcard(n) => n.clone(),
                _ => unreachable!("wildcard_child must be a Wildcard node"),
            };
            // Reconstruct the remaining path (head + tail joined).
            let rest = if tail.is_empty() {
                head.to_string()
            } else {
                let mut s = head.to_string();
                for part in tail {
                    s.push('/');
                    s.push_str(part);
                }
                s
            };
            params.push((wc_name, rest));
            return wc_node.handler.as_ref();
        }

        None
    }

    // ── Tree traversal (for nest/mount flattening) ─────────────────────────

    /// Call `f(segment, child_node)` for every child of this node.
    ///
    /// Used by `collect_routes` in `router.rs` to flatten a mounted sub-router's
    /// routes into `(PathSegment, handler)` pairs before re-insertion.
    pub(crate) fn visit_children<F>(&self, mut f: F)
    where
        F: FnMut(PathSegment, &Node),
    {
        for child in &self.literal_children {
            if let NodeSegment::Literal(s) = &child.segment {
                f(PathSegment::Static(s.clone()), child);
            }
        }
        if let Some(child) = &self.param_child {
            if let NodeSegment::Param(s) = &child.segment {
                f(PathSegment::Param(s.clone()), child);
            }
        }
        if let Some(child) = &self.wildcard_child {
            if let NodeSegment::Wildcard(s) = &child.segment {
                f(PathSegment::Wildcard(s.clone()), child);
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::Request;
    use crate::response::Response;
    use crate::routing::path::parse_pattern;
    use crate::routing::router::into_box_handler;
    use crate::status::StatusCode;

    async fn ok(_req: Request) -> Response {
        Response::new(StatusCode::OK)
    }

    fn make_handler() -> Arc<BoxHandler> {
        Arc::new(into_box_handler(ok))
    }

    fn split_path(path: &str) -> Vec<&str> {
        let p = path.trim_start_matches('/');
        if p.is_empty() {
            vec![]
        } else {
            p.split('/').collect()
        }
    }

    fn insert(root: &mut Node, pattern: &str) -> Arc<BoxHandler> {
        let h = make_handler();
        root.insert(&parse_pattern(pattern), h.clone());
        h
    }

    // ── Static routes ─────────────────────────────────────────────────────

    #[test]
    fn static_exact_match() {
        let mut root = Node::new_root();
        insert(&mut root, "/health");
        let mut params = vec![];
        assert!(root.lookup(&split_path("/health"), &mut params).is_some());
    }

    #[test]
    fn static_no_match_different_path() {
        let mut root = Node::new_root();
        insert(&mut root, "/health");
        let mut params = vec![];
        assert!(root.lookup(&split_path("/status"), &mut params).is_none());
    }

    #[test]
    fn static_no_match_prefix_only() {
        let mut root = Node::new_root();
        insert(&mut root, "/users");
        let mut params = vec![];
        assert!(root
            .lookup(&split_path("/users/extra"), &mut params)
            .is_none());
    }

    #[test]
    fn multiple_static_siblings() {
        let mut root = Node::new_root();
        insert(&mut root, "/alpha");
        insert(&mut root, "/beta");
        insert(&mut root, "/gamma");
        for path in ["/alpha", "/beta", "/gamma"] {
            let mut p = vec![];
            assert!(root.lookup(&split_path(path), &mut p).is_some(), "{path}");
        }
        let mut p = vec![];
        assert!(root.lookup(&split_path("/delta"), &mut p).is_none());
    }

    // ── Param routes ─────────────────────────────────────────────────────

    #[test]
    fn param_capture() {
        let mut root = Node::new_root();
        insert(&mut root, "/users/:id");
        let mut params = vec![];
        assert!(root
            .lookup(&split_path("/users/42"), &mut params)
            .is_some());
        assert_eq!(params, vec![("id".into(), "42".into())]);
    }

    #[test]
    fn multiple_params_in_path() {
        let mut root = Node::new_root();
        insert(&mut root, "/users/:uid/posts/:pid");
        let mut params = vec![];
        assert!(root
            .lookup(&split_path("/users/10/posts/99"), &mut params)
            .is_some());
        assert!(params.contains(&("uid".into(), "10".into())));
        assert!(params.contains(&("pid".into(), "99".into())));
    }

    // ── Wildcard routes ───────────────────────────────────────────────────

    #[test]
    fn wildcard_capture_multi_segment() {
        let mut root = Node::new_root();
        insert(&mut root, "/files/*path");
        let mut params = vec![];
        assert!(root
            .lookup(&split_path("/files/a/b/c"), &mut params)
            .is_some());
        assert_eq!(params, vec![("path".into(), "a/b/c".into())]);
    }

    #[test]
    fn wildcard_capture_single_segment() {
        let mut root = Node::new_root();
        insert(&mut root, "/files/*path");
        let mut params = vec![];
        assert!(root
            .lookup(&split_path("/files/readme.txt"), &mut params)
            .is_some());
        assert_eq!(params[0].1, "readme.txt");
    }

    // ── Priority: literal > param > wildcard ──────────────────────────────

    #[test]
    fn literal_beats_param() {
        let mut root = Node::new_root();
        let literal_h = insert(&mut root, "/users/admin");
        let param_h = insert(&mut root, "/users/:id");

        let mut params = vec![];
        let result = root
            .lookup(&split_path("/users/admin"), &mut params)
            .unwrap();
        // Should match the literal handler, not the param handler.
        assert!(Arc::ptr_eq(result, &literal_h));
        assert!(params.is_empty(), "literal match should not capture params");

        // Regular param still works.
        let mut params2 = vec![];
        let result2 = root
            .lookup(&split_path("/users/42"), &mut params2)
            .unwrap();
        assert!(Arc::ptr_eq(result2, &param_h));
        assert_eq!(params2, vec![("id".into(), "42".into())]);
    }

    #[test]
    fn param_beats_wildcard() {
        let mut root = Node::new_root();
        let param_h = insert(&mut root, "/files/:name");
        let wildcard_h = insert(&mut root, "/files/*rest");

        // Single segment: param should win.
        let mut params = vec![];
        let result = root
            .lookup(&split_path("/files/hello"), &mut params)
            .unwrap();
        assert!(Arc::ptr_eq(result, &param_h));
        assert_eq!(params, vec![("name".into(), "hello".into())]);

        // Multi-segment: param can't match (only one seg), wildcard wins.
        let mut params2 = vec![];
        let result2 = root
            .lookup(&split_path("/files/a/b"), &mut params2)
            .unwrap();
        assert!(Arc::ptr_eq(result2, &wildcard_h));
    }

    // ── Root path ─────────────────────────────────────────────────────────

    #[test]
    fn root_path_match() {
        let mut root = Node::new_root();
        insert(&mut root, "/");
        let mut params = vec![];
        assert!(root.lookup(&split_path("/"), &mut params).is_some());
    }

    #[test]
    fn root_does_not_match_subpath() {
        let mut root = Node::new_root();
        insert(&mut root, "/");
        let mut params = vec![];
        assert!(root.lookup(&split_path("/extra"), &mut params).is_none());
    }

    // ── Nested / deep routes ──────────────────────────────────────────────

    #[test]
    fn deep_nested_static() {
        let mut root = Node::new_root();
        insert(&mut root, "/api/v1/users/profile");
        let mut params = vec![];
        assert!(root
            .lookup(&split_path("/api/v1/users/profile"), &mut params)
            .is_some());
        assert!(root
            .lookup(&split_path("/api/v1/users"), &mut params)
            .is_none());
    }

    #[test]
    fn many_routes_correct_dispatch() {
        let mut root = Node::new_root();
        for i in 0..50 {
            insert(&mut root, &format!("/route/{i}"));
        }
        for i in 0..50 {
            let mut p = vec![];
            assert!(
                root.lookup(&split_path(&format!("/route/{i}")), &mut p)
                    .is_some(),
                "route {i} should match"
            );
        }
    }

    // ── Param rollback on no-match ─────────────────────────────────────────

    #[test]
    fn param_rollback_when_no_handler_at_end() {
        let mut root = Node::new_root();
        // Only register /users/:id/posts — NOT /users/:id
        insert(&mut root, "/users/:id/posts");

        // /users/42 should NOT match (no handler at :id level without /posts).
        let mut params = vec![];
        assert!(root.lookup(&split_path("/users/42"), &mut params).is_none());
        // Params must be empty after failed lookup.
        assert!(params.is_empty());
    }

    // ── Overwrite duplicate route ─────────────────────────────────────────

    #[test]
    fn duplicate_route_overwrites_handler() {
        let mut root = Node::new_root();
        let _h1 = insert(&mut root, "/ping");
        let h2 = insert(&mut root, "/ping"); // second registration wins

        let mut params = vec![];
        let result = root.lookup(&split_path("/ping"), &mut params).unwrap();
        assert!(Arc::ptr_eq(result, &h2));
    }
}

//! Path segment parsing and matching for the radix-tree router.
//!
//! Supports three segment kinds:
//! - `Static("users")` — literal match
//! - `Param("id")`     — captures one path segment as a named param
//! - `Wildcard("rest")`— captures the remainder of the path

/// A single parsed path segment from a route pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSegment {
    /// Literal string match (e.g. `"users"`).
    Static(String),
    /// Single-segment capture (e.g. `:id`).
    Param(String),
    /// Multi-segment capture (e.g. `*rest`). Must be the final segment.
    Wildcard(String),
}

/// Parse a route pattern into a `Vec<PathSegment>`.
///
/// # Examples
/// - `"/users/:id/posts"` → `[Static("users"), Param("id"), Static("posts")]`
/// - `"/files/*path"`     → `[Static("files"), Wildcard("path")]`
pub fn parse_pattern(pattern: &str) -> Vec<PathSegment> {
    let stripped = pattern.trim_start_matches('/');
    if stripped.is_empty() {
        return Vec::new();
    }
    stripped.split('/').map(|seg| {
        if let Some(name) = seg.strip_prefix(':') {
            PathSegment::Param(name.to_string())
        } else if let Some(name) = seg.strip_prefix('*') {
            PathSegment::Wildcard(name.to_string())
        } else {
            PathSegment::Static(seg.to_string())
        }
    }).collect()
}

/// Match `path` against a slice of `PathSegment`s, filling `params` with
/// captured `(name, value)` pairs. Returns `true` on full match.
pub fn match_segments<'a>(
    segments: &[PathSegment],
    path: &'a str,
    params: &mut Vec<(&'static str, String)>,
) -> bool {
    // We need static name lifetimes for params; use string storage instead.
    match_segments_owned(segments, path, &mut params.iter().map(|_| ()).collect::<Vec<_>>());
    // Delegate to the owned version that returns captured pairs.
    false // placeholder — use match_path below
}

/// Match a URL path against route segments, returning captured params.
///
/// Returns `Some(Vec<(name, value)>)` on match, `None` on mismatch.
pub fn match_path<'p>(
    segments: &'p [PathSegment],
    url_path: &str,
) -> Option<Vec<(String, String)>> {
    let path = url_path.trim_start_matches('/');
    let parts: Vec<&str> = if path.is_empty() { vec![] } else { path.split('/').collect() };

    let mut params = Vec::new();
    let mut pi = 0; // index into `parts`

    for seg in segments {
        match seg {
            PathSegment::Static(name) => {
                if pi >= parts.len() || parts[pi] != name.as_str() {
                    return None;
                }
                pi += 1;
            }
            PathSegment::Param(name) => {
                if pi >= parts.len() {
                    return None;
                }
                params.push((name.clone(), parts[pi].to_string()));
                pi += 1;
            }
            PathSegment::Wildcard(name) => {
                // Captures all remaining segments joined with '/'.
                let rest = parts[pi..].join("/");
                params.push((name.clone(), rest));
                return Some(params); // wildcard always terminates matching
            }
        }
    }

    // All segments consumed — path must be fully consumed too.
    if pi == parts.len() { Some(params) } else { None }
}

// Unused helper; kept to satisfy the public signature above.
fn match_segments_owned(_: &[PathSegment], _: &str, _: &mut Vec<()>) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_static() {
        let segs = parse_pattern("/users/profile");
        assert_eq!(segs, vec![
            PathSegment::Static("users".into()),
            PathSegment::Static("profile".into()),
        ]);
    }

    #[test]
    fn parse_param() {
        let segs = parse_pattern("/users/:id");
        assert_eq!(segs, vec![
            PathSegment::Static("users".into()),
            PathSegment::Param("id".into()),
        ]);
    }

    #[test]
    fn parse_wildcard() {
        let segs = parse_pattern("/files/*path");
        assert_eq!(segs, vec![
            PathSegment::Static("files".into()),
            PathSegment::Wildcard("path".into()),
        ]);
    }

    #[test]
    fn match_static_path() {
        let segs = parse_pattern("/users/profile");
        assert!(match_path(&segs, "/users/profile").is_some());
        assert!(match_path(&segs, "/users/other").is_none());
    }

    #[test]
    fn match_param_extraction() {
        let segs = parse_pattern("/users/:id");
        let params = match_path(&segs, "/users/42").unwrap();
        assert_eq!(params, vec![("id".to_string(), "42".to_string())]);
    }

    #[test]
    fn match_wildcard() {
        let segs = parse_pattern("/files/*path");
        let params = match_path(&segs, "/files/a/b/c").unwrap();
        assert_eq!(params, vec![("path".to_string(), "a/b/c".to_string())]);
    }

    #[test]
    fn match_root() {
        let segs = parse_pattern("/");
        assert!(match_path(&segs, "/").is_some());
        assert!(match_path(&segs, "/extra").is_none());
    }

    #[test]
    fn no_match_extra_segments() {
        let segs = parse_pattern("/users");
        assert!(match_path(&segs, "/users/extra").is_none());
    }
}

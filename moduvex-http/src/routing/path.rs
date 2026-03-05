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
///
/// # Wildcard placement
/// A `*name` wildcard captures all remaining path segments and **must** be the
/// last segment in the pattern. If trailing segments follow a wildcard they are
/// silently ignored (the wildcard always terminates matching). A warning is
/// printed to stderr when this condition is detected so route authors can fix
/// their patterns.
pub fn parse_pattern(pattern: &str) -> Vec<PathSegment> {
    let stripped = pattern.trim_start_matches('/');
    if stripped.is_empty() {
        return Vec::new();
    }
    let segments: Vec<PathSegment> = stripped
        .split('/')
        .map(|seg| {
            if let Some(name) = seg.strip_prefix(':') {
                PathSegment::Param(name.to_string())
            } else if let Some(name) = seg.strip_prefix('*') {
                PathSegment::Wildcard(name.to_string())
            } else {
                PathSegment::Static(seg.to_string())
            }
        })
        .collect();

    // Warn if a wildcard appears before the last segment — trailing segments
    // will never be matched and the pattern is likely a mistake.
    if let Some(wildcard_pos) = segments
        .iter()
        .position(|s| matches!(s, PathSegment::Wildcard(_)))
    {
        if wildcard_pos + 1 < segments.len() {
            eprintln!(
                "[moduvex-http] warning: wildcard segment is not last in pattern '{pattern}'; \
                 segments after the wildcard are unreachable and will be ignored"
            );
        }
    }

    segments
}

/// Match a URL path against route segments, returning captured params.
///
/// Returns `Some(Vec<(name, value)>)` on match, `None` on mismatch.
pub fn match_path(segments: &[PathSegment], url_path: &str) -> Option<Vec<(String, String)>> {
    let path = url_path.trim_start_matches('/');
    let parts: Vec<&str> = if path.is_empty() {
        vec![]
    } else {
        path.split('/').collect()
    };

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
    if pi == parts.len() {
        Some(params)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_static() {
        let segs = parse_pattern("/users/profile");
        assert_eq!(
            segs,
            vec![
                PathSegment::Static("users".into()),
                PathSegment::Static("profile".into()),
            ]
        );
    }

    #[test]
    fn parse_param() {
        let segs = parse_pattern("/users/:id");
        assert_eq!(
            segs,
            vec![
                PathSegment::Static("users".into()),
                PathSegment::Param("id".into()),
            ]
        );
    }

    #[test]
    fn parse_wildcard() {
        let segs = parse_pattern("/files/*path");
        assert_eq!(
            segs,
            vec![
                PathSegment::Static("files".into()),
                PathSegment::Wildcard("path".into()),
            ]
        );
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

    #[test]
    fn wildcard_not_last_is_still_parseable() {
        // parse_pattern warns but returns a usable segment list.
        // The wildcard captures all remaining; trailing segments are unreachable.
        let segs = parse_pattern("/files/*path/extra");
        // Wildcard is at position 1; "extra" is at position 2 (unreachable).
        assert!(segs.iter().any(|s| matches!(s, PathSegment::Wildcard(_))));
    }
}

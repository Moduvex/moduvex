//! `Path` extractor — access captured path parameters from routing.

use crate::request::Request;
use crate::response::Response;

use super::FromRequest;

// ── Path extractor ───────────────────────────────────────────────────────────

/// Captured path parameters from the router (e.g. `/users/:id` → `id`).
///
/// Access individual parameters via `.get("name")`.
pub struct Path {
    params: Vec<(String, String)>,
}

impl Path {
    /// Get a path parameter by name.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// Consume and return the raw parameter list.
    pub fn into_inner(self) -> Vec<(String, String)> {
        self.params
    }

    /// Number of captured parameters.
    pub fn len(&self) -> usize {
        self.params.len()
    }

    /// True if no parameters were captured.
    pub fn is_empty(&self) -> bool {
        self.params.is_empty()
    }
}

impl FromRequest for Path {
    type Rejection = Response;

    fn from_request(req: &mut Request) -> Result<Self, Self::Rejection> {
        // The router inserts Vec<(String, String)> into extensions on match.
        let params = req
            .extensions
            .remove::<Vec<(String, String)>>()
            .unwrap_or_default();
        Ok(Path { params })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::method::Method;

    #[test]
    fn path_extract_with_params() {
        let mut req = Request::new(Method::GET, "/users/42");
        req.extensions
            .insert(vec![("id".to_string(), "42".to_string())]);
        let path = Path::from_request(&mut req).unwrap();
        assert_eq!(path.get("id"), Some("42"));
        assert_eq!(path.len(), 1);
    }

    #[test]
    fn path_extract_without_params() {
        let mut req = Request::new(Method::GET, "/");
        let path = Path::from_request(&mut req).unwrap();
        assert!(path.is_empty());
    }

    #[test]
    fn path_extract_multiple_keys() {
        let mut req = Request::new(Method::GET, "/users/42/posts/7");
        req.extensions.insert(vec![
            ("uid".to_string(), "42".to_string()),
            ("pid".to_string(), "7".to_string()),
        ]);
        let path = Path::from_request(&mut req).unwrap();
        assert_eq!(path.get("uid"), Some("42"));
        assert_eq!(path.get("pid"), Some("7"));
        assert_eq!(path.len(), 2);
    }

    #[test]
    fn path_get_missing_key_returns_none() {
        let mut req = Request::new(Method::GET, "/users/1");
        req.extensions
            .insert(vec![("id".to_string(), "1".to_string())]);
        let path = Path::from_request(&mut req).unwrap();
        assert!(path.get("missing_key").is_none());
    }

    #[test]
    fn path_into_inner_returns_params() {
        let mut req = Request::new(Method::GET, "/item/42");
        req.extensions
            .insert(vec![("id".to_string(), "42".to_string())]);
        let path = Path::from_request(&mut req).unwrap();
        let inner = path.into_inner();
        assert_eq!(inner, vec![("id".to_string(), "42".to_string())]);
    }

    #[test]
    fn path_params_consumed_from_extensions() {
        // After extraction, params are removed from extensions
        let mut req = Request::new(Method::GET, "/x/1");
        req.extensions
            .insert(vec![("x".to_string(), "1".to_string())]);
        let _ = Path::from_request(&mut req).unwrap();
        // Params are removed (consumed)
        assert!(req.extensions.get::<Vec<(String, String)>>().is_none());
    }
}

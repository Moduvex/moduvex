//! Static file serving middleware — maps URL path prefixes to filesystem directories.
//!
//! # Security
//! Paths are canonicalized after joining with the base directory.
//! The canonicalized path is then verified to start with the canonicalized
//! base directory to prevent directory traversal attacks (e.g. `../../etc/passwd`).
//!
//! # Usage
//! ```ignore
//! use moduvex_http::middleware::builtin::StaticFiles;
//!
//! HttpServer::bind("0.0.0.0:8080")
//!     .middleware(StaticFiles::new("/assets", "./public"))
//!     .serve();
//! ```

use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::future::Future;

use crate::middleware::{Middleware, Next};
use crate::request::Request;
use crate::response::Response;
use crate::status::StatusCode;

// ── MIME type table ────────────────────────────────────────────────────────────

/// Return the MIME type for a file extension.
///
/// Covers common web file types. Unknown extensions default to
/// `application/octet-stream` (forces download rather than rendering).
fn mime_for_extension(ext: &str) -> &'static str {
    match ext.to_ascii_lowercase().as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css"          => "text/css; charset=utf-8",
        "js" | "mjs"   => "application/javascript; charset=utf-8",
        "json"         => "application/json",
        "xml"          => "application/xml",
        "txt"          => "text/plain; charset=utf-8",
        "svg"          => "image/svg+xml",
        "png"          => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif"          => "image/gif",
        "webp"         => "image/webp",
        "ico"          => "image/x-icon",
        "wasm"         => "application/wasm",
        "pdf"          => "application/pdf",
        "zip"          => "application/zip",
        "ttf"          => "font/ttf",
        "woff"         => "font/woff",
        "woff2"        => "font/woff2",
        _              => "application/octet-stream",
    }
}

// ── StaticFiles middleware ─────────────────────────────────────────────────────

/// Middleware that serves files from `base_dir` under the URL `prefix`.
///
/// A request for `/assets/style.css` with prefix `/assets` and
/// `base_dir = "./public"` serves the file at `./public/style.css`.
///
/// Responds with:
/// - `200 OK` + file body + `Content-Type` + `Content-Length` + `Cache-Control`
/// - `403 Forbidden` if the path is a directory (no listing)
/// - `404 Not Found` if the file does not exist
/// - Passes through to the next handler if the URL does not start with `prefix`
pub struct StaticFiles {
    /// URL path prefix (e.g. `/assets`).
    prefix: String,
    /// Canonicalized base directory on the filesystem.
    base_dir: PathBuf,
}

impl StaticFiles {
    /// Create a new `StaticFiles` middleware.
    ///
    /// `prefix` is the URL path prefix (e.g. `"/assets"`).
    /// `base_dir` is the filesystem directory to serve from (e.g. `"./public"`).
    ///
    /// # Errors
    /// Returns `Err` if `base_dir` cannot be canonicalized (does not exist or
    /// is not a directory).
    pub fn new(prefix: impl Into<String>, base_dir: impl AsRef<Path>) -> std::io::Result<Self> {
        let base = std::fs::canonicalize(base_dir.as_ref())?;
        if !base.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("base_dir '{}' is not a directory", base.display()),
            ));
        }
        Ok(Self {
            prefix: prefix.into(),
            base_dir: base,
        })
    }

    /// Try to serve a static file for the given URL path.
    ///
    /// Returns `Some(response)` if the path starts with the prefix,
    /// `None` if the request should fall through to the next handler.
    fn try_serve(&self, url_path: &str) -> Option<Response> {
        // Only handle requests that start with our prefix.
        let rel = url_path.strip_prefix(self.prefix.trim_end_matches('/'))?;

        // Strip leading slash so we get a relative path.
        let rel = rel.trim_start_matches('/');

        // Build the candidate path: base_dir / rel_path
        let candidate = self.base_dir.join(rel);

        // Canonicalize to resolve `..`, symlinks, etc.
        // If canonicalization fails (path doesn't exist), return 404.
        let canonical = match std::fs::canonicalize(&candidate) {
            Ok(p)  => p,
            Err(_) => return Some(Response::not_found()),
        };

        // Security: ensure the canonical path is inside base_dir.
        if !canonical.starts_with(&self.base_dir) {
            return Some(Response::with_body(
                StatusCode::FORBIDDEN,
                "403 Forbidden",
            ).content_type("text/plain; charset=utf-8"));
        }

        // Reject directories (no listing).
        if canonical.is_dir() {
            return Some(Response::with_body(
                StatusCode::FORBIDDEN,
                "403 Forbidden — directory listing disabled",
            ).content_type("text/plain; charset=utf-8"));
        }

        // Read the file (blocking — acceptable for MVP; async file I/O in v0.3).
        let bytes = match std::fs::read(&canonical) {
            Ok(b)  => b,
            Err(_) => return Some(Response::not_found()),
        };

        // Determine MIME type from file extension.
        let mime = canonical
            .extension()
            .and_then(|e| e.to_str())
            .map(mime_for_extension)
            .unwrap_or("application/octet-stream");

        // Build Last-Modified header from file metadata.
        let last_modified = std::fs::metadata(&canonical)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| {
                t.duration_since(std::time::UNIX_EPOCH).ok()
            })
            .map(|d| format!("{}", d.as_secs()));

        let content_len = bytes.len().to_string();
        let mut resp = Response::with_body(StatusCode::OK, bytes)
            .content_type(mime);
        resp.headers.insert("content-length", content_len.into_bytes());
        // Cache-Control: 1 hour for static assets.
        resp.headers.insert("cache-control", b"public, max-age=3600".to_vec());
        if let Some(lm) = last_modified {
            resp.headers.insert("last-modified", lm.into_bytes());
        }
        Some(resp)
    }
}

impl Middleware for StaticFiles {
    fn handle(&self, req: Request, next: Next) -> Pin<Box<dyn Future<Output = Response> + Send>> {
        // Clone the prefix and base_dir for use in the async block.
        // `try_serve` is synchronous, so we call it immediately before boxing.
        let response = self.try_serve(&req.path);
        Box::pin(async move {
            match response {
                Some(r) => r,
                None    => next.run(req).await,
            }
        })
    }
}

// ── Router extension ──────────────────────────────────────────────────────────
//
// `Router::static_files` is intentionally kept out of this file to avoid a
// circular dependency between the router and middleware modules. Users should
// mount `StaticFiles` via `.middleware(StaticFiles::new(...))` instead.
// A `Router::static_files` convenience method can be layered on top if desired.

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir_with_files() -> (tempfile_helper::TempDir, PathBuf) {
        let dir = tempfile_helper::TempDir::new();
        fs::write(dir.path().join("hello.txt"), b"hello world").unwrap();
        fs::write(dir.path().join("page.html"), b"<h1>Hi</h1>").unwrap();
        fs::create_dir(dir.path().join("subdir")).unwrap();
        let path = dir.path().to_path_buf();
        (dir, path)
    }

    mod tempfile_helper {
        use std::path::{Path, PathBuf};

        pub struct TempDir(PathBuf);

        impl TempDir {
            pub fn new() -> Self {
                let path = std::env::temp_dir()
                    .join(format!("moduvex_test_{}", std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .subsec_nanos()));
                std::fs::create_dir_all(&path).unwrap();
                Self(path)
            }

            pub fn path(&self) -> &Path {
                &self.0
            }
        }

        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    #[test]
    fn mime_for_known_extensions() {
        assert_eq!(mime_for_extension("html"), "text/html; charset=utf-8");
        assert_eq!(mime_for_extension("css"),  "text/css; charset=utf-8");
        assert_eq!(mime_for_extension("js"),   "application/javascript; charset=utf-8");
        assert_eq!(mime_for_extension("json"), "application/json");
        assert_eq!(mime_for_extension("png"),  "image/png");
        assert_eq!(mime_for_extension("wasm"), "application/wasm");
    }

    #[test]
    fn mime_for_unknown_extension_is_octet_stream() {
        assert_eq!(mime_for_extension("xyz"), "application/octet-stream");
        assert_eq!(mime_for_extension(""),    "application/octet-stream");
    }

    #[test]
    fn serve_existing_file() {
        let (_dir, base) = temp_dir_with_files();
        let sf = StaticFiles::new("/assets", &base).unwrap();

        let resp = sf.try_serve("/assets/hello.txt").unwrap();
        assert_eq!(resp.status, StatusCode::OK);
        assert!(resp.headers.get_str("content-type")
            .unwrap_or("").contains("text/plain"));
        assert_eq!(resp.body.into_bytes(), b"hello world");
    }

    #[test]
    fn serve_html_file_with_correct_mime() {
        let (_dir, base) = temp_dir_with_files();
        let sf = StaticFiles::new("/static", &base).unwrap();

        let resp = sf.try_serve("/static/page.html").unwrap();
        assert_eq!(resp.status, StatusCode::OK);
        assert!(resp.headers.get_str("content-type")
            .unwrap_or("").contains("text/html"));
    }

    #[test]
    fn missing_file_returns_404() {
        let (_dir, base) = temp_dir_with_files();
        let sf = StaticFiles::new("/assets", &base).unwrap();

        let resp = sf.try_serve("/assets/missing.txt").unwrap();
        assert_eq!(resp.status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn directory_path_returns_403() {
        let (_dir, base) = temp_dir_with_files();
        let sf = StaticFiles::new("/assets", &base).unwrap();

        let resp = sf.try_serve("/assets/subdir").unwrap();
        assert_eq!(resp.status, StatusCode::FORBIDDEN);
    }

    #[test]
    fn path_traversal_is_blocked() {
        let (_dir, base) = temp_dir_with_files();
        let sf = StaticFiles::new("/assets", &base).unwrap();

        // Attempt to escape base_dir via ../
        let resp = sf.try_serve("/assets/../../../etc/passwd");
        // Either None (prefix mismatch after stripping) or 404/403.
        // The canonicalize step will resolve the traversal and the
        // starts_with check will reject it.
        match resp {
            None => {}  // prefix didn't match (legitimate)
            Some(r) => {
                assert!(
                    r.status == StatusCode::NOT_FOUND
                        || r.status == StatusCode::FORBIDDEN,
                    "expected 404 or 403, got {:?}", r.status
                );
            }
        }
    }

    #[test]
    fn unmatched_prefix_returns_none() {
        let (_dir, base) = temp_dir_with_files();
        let sf = StaticFiles::new("/assets", &base).unwrap();

        // Request for /api/users should not match /assets prefix.
        assert!(sf.try_serve("/api/users").is_none());
    }

    #[test]
    fn cache_control_header_present() {
        let (_dir, base) = temp_dir_with_files();
        let sf = StaticFiles::new("/assets", &base).unwrap();

        let resp = sf.try_serve("/assets/hello.txt").unwrap();
        assert_eq!(
            resp.headers.get_str("cache-control"),
            Some("public, max-age=3600")
        );
    }

    #[test]
    fn content_length_header_present() {
        let (_dir, base) = temp_dir_with_files();
        let sf = StaticFiles::new("/assets", &base).unwrap();

        let resp = sf.try_serve("/assets/hello.txt").unwrap();
        assert_eq!(
            resp.headers.get_str("content-length"),
            Some("11") // "hello world" is 11 bytes
        );
    }

    #[test]
    fn new_with_nonexistent_dir_returns_error() {
        let result = StaticFiles::new("/x", "/nonexistent/directory/xyz");
        assert!(result.is_err());
    }
}

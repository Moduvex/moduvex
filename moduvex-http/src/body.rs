//! HTTP request/response body abstraction.
//!
//! Three variants cover all practical cases without unnecessary complexity:
//! - `Empty`  — no body (GET, HEAD, 204, etc.)
//! - `Fixed`  — fully buffered bytes (small JSON payloads, form data)
//! - `Stream` — channel-backed iterator for large or streamed bodies

use std::sync::{Arc, Mutex};
use std::collections::VecDeque;

// ── Body ──────────────────────────────────────────────────────────────────────

/// HTTP body — owned bytes or a streaming channel.
#[derive(Debug)]
pub enum Body {
    /// No body content.
    Empty,
    /// Fully buffered body — all bytes in memory.
    Fixed(Vec<u8>),
    /// Streaming body — data arrives via a [`BodySender`].
    Stream(BodyReceiver),
}

impl Body {
    /// Construct an empty body.
    pub fn empty() -> Self { Self::Empty }

    /// Construct from a byte slice, copying the data.
    pub fn from_bytes(bytes: impl Into<Vec<u8>>) -> Self {
        let v = bytes.into();
        if v.is_empty() { Self::Empty } else { Self::Fixed(v) }
    }

    /// Construct from a UTF-8 string.
    pub fn from_text(text: impl Into<String>) -> Self {
        Self::from_bytes(text.into().into_bytes())
    }

    /// Create a new streaming body; returns `(Body, sender)`.
    pub fn channel() -> (Self, BodySender) {
        let inner = Arc::new(Mutex::new(ChannelInner {
            chunks: VecDeque::new(),
            closed: false,
        }));
        let receiver = BodyReceiver { inner: inner.clone() };
        let sender = BodySender { inner };
        (Self::Stream(receiver), sender)
    }

    /// Consume body, collecting all bytes into a `Vec<u8>`.
    ///
    /// For streaming bodies this drains what has been pushed so far.
    /// For a `Fixed` body this is zero-copy (moves the `Vec`).
    pub fn into_bytes(self) -> Vec<u8> {
        match self {
            Self::Empty => Vec::new(),
            Self::Fixed(v) => v,
            Self::Stream(rx) => rx.collect(),
        }
    }

    /// Content length if known statically (`Fixed` bodies only).
    pub fn content_length(&self) -> Option<usize> {
        match self {
            Self::Empty => Some(0),
            Self::Fixed(v) => Some(v.len()),
            Self::Stream(_) => None,
        }
    }

    /// True if body carries no bytes.
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Empty => true,
            Self::Fixed(v) => v.is_empty(),
            Self::Stream(_) => false,
        }
    }
}

impl Default for Body {
    fn default() -> Self { Self::Empty }
}

impl From<Vec<u8>> for Body {
    fn from(v: Vec<u8>) -> Self { Self::from_bytes(v) }
}

impl From<&[u8]> for Body {
    fn from(s: &[u8]) -> Self { Self::from_bytes(s.to_vec()) }
}

impl From<String> for Body {
    fn from(s: String) -> Self { Self::from_text(s) }
}

impl From<&str> for Body {
    fn from(s: &str) -> Self { Self::from_text(s) }
}

// ── Streaming channel internals ───────────────────────────────────────────────

#[derive(Debug)]
struct ChannelInner {
    chunks: VecDeque<Vec<u8>>,
    closed: bool,
}

/// Write-end of a streaming body channel.
pub struct BodySender {
    inner: Arc<Mutex<ChannelInner>>,
}

impl BodySender {
    /// Push a chunk into the stream.
    pub fn send(&self, chunk: Vec<u8>) {
        if let Ok(mut guard) = self.inner.lock() {
            if !guard.closed {
                guard.chunks.push_back(chunk);
            }
        }
    }

    /// Close the stream — no more chunks can be pushed.
    pub fn close(self) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.closed = true;
        }
    }
}

/// Read-end of a streaming body channel.
#[derive(Debug)]
pub struct BodyReceiver {
    inner: Arc<Mutex<ChannelInner>>,
}

impl BodyReceiver {
    /// Pull the next chunk from the stream, or `None` if closed and empty.
    pub fn next_chunk(&self) -> Option<Vec<u8>> {
        let mut guard = self.inner.lock().ok()?;
        guard.chunks.pop_front()
    }

    /// Drain all available chunks into a single `Vec<u8>`.
    pub fn collect(self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Ok(mut guard) = self.inner.lock() {
            for chunk in guard.chunks.drain(..) {
                out.extend_from_slice(&chunk);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_body() {
        let b = Body::empty();
        assert!(b.is_empty());
        assert_eq!(b.content_length(), Some(0));
    }

    #[test]
    fn fixed_body_from_str() {
        let b = Body::from("hello");
        assert!(!b.is_empty());
        assert_eq!(b.content_length(), Some(5));
        assert_eq!(b.into_bytes(), b"hello");
    }

    #[test]
    fn channel_send_recv() {
        let (body, sender) = Body::channel();
        sender.send(b"chunk1".to_vec());
        sender.send(b"chunk2".to_vec());
        sender.close();
        assert_eq!(body.into_bytes(), b"chunk1chunk2");
    }

    #[test]
    fn body_from_vec() {
        let b: Body = vec![1u8, 2, 3].into();
        assert_eq!(b.content_length(), Some(3));
    }

    #[test]
    fn empty_vec_becomes_empty() {
        let b = Body::from_bytes(vec![]);
        assert!(matches!(b, Body::Empty));
    }
}

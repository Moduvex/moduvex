//! HTTP/2 protocol implementation (RFC 9113).
//!
//! Module layout:
//! - `frame`        — binary frame codec (parse + encode, all 10 types)
//! - `error`        — `H2Error` / `H2ErrorCode` types
//! - `hpack`        — HPACK header compression (RFC 7541)
//! - `stream`       — per-stream state machine (RFC 9113 §5.1)
//! - `flow_control` — connection-level flow-control windows (RFC 9113 §5.2)
//! - `connection`   — connection manager: stream table, settings, request/response I/O
//! - `connection_io`— async I/O helpers (frame read/write, SETTINGS apply)

pub mod connection;
pub mod connection_io;
pub mod error;
pub mod flow_control;
pub mod frame;
pub mod hpack;
pub mod stream;

pub use connection::{H2Connection, H2Settings, H2_PREFACE};
pub use error::{H2Error, H2ErrorCode};
pub use flow_control::{FlowController, DEFAULT_WINDOW_SIZE};
pub use frame::{Frame, FrameHeader, FrameType};
pub use stream::{H2Stream, StreamState};

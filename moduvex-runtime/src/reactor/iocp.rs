//! Windows IOCP reactor backend — stub implementation.
//!
//! TODO: Implement AFD-based IOCP reactor — see mio sys/windows/
//! The proper implementation requires:
//! - `CreateIoCompletionPort` for the IOCP handle
//! - AFD (Ancillary Function Driver) poll for socket readiness
//! - `GetQueuedCompletionStatusEx` in the poll loop
//! - Overlapped I/O structures per registered source
//!
//! This file is only compiled on Windows — the outer `mod iocp` declaration
//! in `reactor/mod.rs` is guarded by `#[cfg(target_os = "windows")]`.

use std::io;

use crate::platform::sys::{Events, Interest, RawSource};
use super::ReactorBackend;

/// Windows IOCP-based reactor backend (not yet implemented).
pub(crate) struct IocpReactor {
    /// IOCP completion port handle — placeholder field.
    _iocp: windows_sys::Win32::Foundation::HANDLE,
}

impl ReactorBackend for IocpReactor {
    fn new() -> io::Result<Self> {
        // TODO: Implement AFD-based IOCP reactor — see mio sys/windows/
        todo!("IocpReactor::new — IOCP backend not yet implemented")
    }

    fn register(&self, _source: RawSource, _token: usize, _interest: Interest) -> io::Result<()> {
        // TODO: Implement AFD-based IOCP reactor — see mio sys/windows/
        todo!("IocpReactor::register")
    }

    fn reregister(
        &self,
        _source: RawSource,
        _token: usize,
        _interest: Interest,
    ) -> io::Result<()> {
        // TODO: Implement AFD-based IOCP reactor — see mio sys/windows/
        todo!("IocpReactor::reregister")
    }

    fn deregister(&self, _source: RawSource) -> io::Result<()> {
        // TODO: Implement AFD-based IOCP reactor — see mio sys/windows/
        todo!("IocpReactor::deregister")
    }

    fn poll(&self, _events: &mut Events, _timeout_ms: Option<u64>) -> io::Result<usize> {
        // TODO: Implement AFD-based IOCP reactor — see mio sys/windows/
        todo!("IocpReactor::poll")
    }
}

impl Drop for IocpReactor {
    fn drop(&mut self) {
        // TODO: CloseHandle(self._iocp) when implemented.
    }
}

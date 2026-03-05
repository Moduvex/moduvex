//! Platform-specific I/O primitives.
//!
//! Provides a thin, cross-platform surface over OS I/O handles, interest flags,
//! and event types. Unix uses `libc` raw file descriptors; Windows stubs use
//! `windows-sys` HANDLE types.

use std::io;

// ── Unix ─────────────────────────────────────────────────────────────────────

#[cfg(unix)]
use std::os::unix::io::RawFd;

/// Raw I/O handle type.
/// - Unix:   `i32` (raw file descriptor)
/// - Windows: `isize` (HANDLE via windows-sys)
#[cfg(unix)]
pub type RawSource = RawFd;

#[cfg(windows)]
pub type RawSource = windows_sys::Win32::Foundation::HANDLE;

// ── Interest flags ────────────────────────────────────────────────────────────

/// Bitmask describing which I/O events a source is interested in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interest(u8);

impl Interest {
    /// Register interest in read-readiness.
    pub const READABLE: Interest = Interest(0b0000_0001);
    /// Register interest in write-readiness.
    pub const WRITABLE: Interest = Interest(0b0000_0010);

    /// Returns `true` if the READABLE bit is set.
    #[inline]
    pub fn is_readable(self) -> bool {
        self.0 & Self::READABLE.0 != 0
    }

    /// Returns `true` if the WRITABLE bit is set.
    #[inline]
    pub fn is_writable(self) -> bool {
        self.0 & Self::WRITABLE.0 != 0
    }

    /// Returns the raw bitmask value.
    #[inline]
    pub(crate) fn bits(self) -> u8 {
        self.0
    }
}

impl std::ops::BitOr for Interest {
    type Output = Interest;
    #[inline]
    fn bitor(self, rhs: Self) -> Self::Output {
        Interest(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for Interest {
    #[inline]
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

// ── Event ─────────────────────────────────────────────────────────────────────

/// A single I/O readiness event returned from a `poll` call.
#[derive(Debug, Clone, Copy)]
pub struct Event {
    /// Caller-provided token identifying the I/O source.
    pub token: usize,
    /// True when the source is ready for reading.
    pub readable: bool,
    /// True when the source is ready for writing.
    pub writable: bool,
}

impl Event {
    #[inline]
    pub(crate) fn new(token: usize, readable: bool, writable: bool) -> Self {
        Self {
            token,
            readable,
            writable,
        }
    }
}

/// Collection of events returned from a single `poll` call.
/// Pre-allocated with a reasonable default capacity to avoid realloc on the
/// hot path.
pub type Events = Vec<Event>;

/// Create a fresh `Events` buffer with the given capacity pre-allocated.
#[inline]
pub fn events_with_capacity(cap: usize) -> Events {
    Vec::with_capacity(cap)
}

// ── Unix helpers ──────────────────────────────────────────────────────────────

#[cfg(unix)]
mod unix_impl {
    use super::*;
    use libc::{c_int, fcntl, F_GETFL, F_SETFL, O_NONBLOCK};

    /// Set a file descriptor to non-blocking mode.
    ///
    /// # Errors
    /// Returns `io::Error` if `fcntl` fails.
    pub fn set_nonblocking(fd: RawSource) -> io::Result<()> {
        // SAFETY: `fd` is a valid open file descriptor supplied by the caller.
        // `fcntl(F_GETFL)` is read-only and always safe to call on a valid fd.
        let flags = unsafe { fcntl(fd, F_GETFL) };
        if flags == -1 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` is valid, `flags` was obtained from `F_GETFL` above,
        // and OR-ing with `O_NONBLOCK` is a documented, supported operation.
        let rc = unsafe { fcntl(fd, F_SETFL, flags | O_NONBLOCK) };
        if rc == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Close a file descriptor.
    ///
    /// # Errors
    /// Returns `io::Error` if `close` fails (e.g. EBADF, EIO).
    pub fn close_fd(fd: RawSource) -> io::Result<()> {
        // SAFETY: `fd` is a valid open file descriptor. After this call the fd
        // is invalid and must not be used again — callers are responsible for
        // ensuring this via RAII (Drop impls).
        let rc = unsafe { libc::close(fd) };
        if rc == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Create an OS pipe and return `(read_fd, write_fd)`.
    ///
    /// Both ends are set to `O_NONBLOCK` before returning.
    ///
    /// # Errors
    /// Returns `io::Error` if `pipe` or `set_nonblocking` fails.
    pub fn create_pipe() -> io::Result<(RawSource, RawSource)> {
        let mut fds: [c_int; 2] = [0; 2];
        // SAFETY: `fds` is a stack-allocated array of the size required by
        // `pipe(2)`. On success the kernel writes exactly two valid fds into it.
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        if rc == -1 {
            return Err(io::Error::last_os_error());
        }
        let (r, w) = (fds[0], fds[1]);
        set_nonblocking(r)?;
        set_nonblocking(w)?;
        Ok((r, w))
    }
}

#[cfg(unix)]
pub use unix_impl::{close_fd, create_pipe, set_nonblocking};

// ── Windows implementations ───────────────────────────────────────────────────

#[cfg(windows)]
mod windows_impl {
    use super::*;

    /// Set a Winsock socket to non-blocking mode using `ioctlsocket(FIONBIO)`.
    ///
    /// `handle` is treated as a `SOCKET` (which is `usize` on 64-bit Windows).
    ///
    /// # Safety
    /// Caller must ensure `handle` is a valid open socket descriptor.
    pub fn set_nonblocking(handle: RawSource) -> io::Result<()> {
        // FIONBIO with value 1 enables non-blocking mode on a Winsock socket.
        let mut nonblocking: u32 = 1;
        // SAFETY: `handle` is a valid SOCKET cast to isize (RawSource = HANDLE = isize).
        // `ioctlsocket` is safe to call with a valid socket and FIONBIO command.
        let ret = unsafe {
            windows_sys::Win32::Networking::WinSock::ioctlsocket(
                handle as usize, // SOCKET is usize on 64-bit Windows
                windows_sys::Win32::Networking::WinSock::FIONBIO as i32,
                &mut nonblocking,
            )
        };
        if ret != 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Close an OS handle via `CloseHandle`.
    ///
    /// # Safety
    /// Caller must ensure `handle` is a valid, open HANDLE that has not been
    /// closed already. After this call the handle is invalid.
    pub fn close_fd(handle: RawSource) -> io::Result<()> {
        // SAFETY: `handle` is a valid HANDLE. CloseHandle is the documented
        // way to release kernel resources associated with any HANDLE type.
        let ok = unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Create an anonymous pipe returning `(read_handle, write_handle)`.
    ///
    /// Uses `CreatePipe` with default security attributes (non-inheritable).
    /// The returned handles are OS `HANDLE` values suitable for read/write.
    ///
    /// # Note
    /// Anonymous pipes on Windows are not waitable via `WSAPoll` — they are
    /// primarily used for the executor self-pipe wakeup mechanism where the
    /// write side is signalled and the read side is drained. For reactor
    /// readiness polling, prefer socket pairs or named pipes.
    pub fn create_pipe() -> io::Result<(RawSource, RawSource)> {
        let mut read_handle: RawSource = 0;
        let mut write_handle: RawSource = 0;
        // SAFETY: Both handle pointers are valid stack variables. `CreatePipe`
        // writes valid HANDLE values into them on success (return value != 0).
        // NULL security attributes uses the default descriptor; pipe size 0
        // uses the system default buffer size.
        let ok = unsafe {
            windows_sys::Win32::System::Pipes::CreatePipe(
                &mut read_handle,
                &mut write_handle,
                std::ptr::null(), // default security attributes (non-inheritable)
                0,                // default system buffer size
            )
        };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok((read_handle, write_handle))
        }
    }
}

#[cfg(windows)]
pub use windows_impl::{close_fd, create_pipe, set_nonblocking};

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interest_readable_bit() {
        assert!(Interest::READABLE.is_readable());
        assert!(!Interest::READABLE.is_writable());
    }

    #[test]
    fn interest_writable_bit() {
        assert!(Interest::WRITABLE.is_writable());
        assert!(!Interest::WRITABLE.is_readable());
    }

    #[test]
    fn interest_bitor() {
        let both = Interest::READABLE | Interest::WRITABLE;
        assert!(both.is_readable());
        assert!(both.is_writable());
    }

    #[test]
    fn event_fields() {
        let e = Event::new(42, true, false);
        assert_eq!(e.token, 42);
        assert!(e.readable);
        assert!(!e.writable);
    }

    #[test]
    fn events_capacity() {
        let ev = events_with_capacity(64);
        assert_eq!(ev.len(), 0);
        assert!(ev.capacity() >= 64);
    }

    #[cfg(unix)]
    #[test]
    fn create_pipe_returns_valid_fds() {
        let (r, w) = create_pipe().expect("pipe creation failed");
        // Write one byte and read it back to prove the fds are connected.
        let byte: u8 = 0xAB;
        // SAFETY: `w` is a valid write-end fd; `&byte` is a valid 1-byte buffer.
        let written = unsafe { libc::write(w, &byte as *const u8 as *const _, 1) };
        assert_eq!(written, 1);
        let mut buf: u8 = 0;
        // SAFETY: `r` is the corresponding read-end fd; `&mut buf` is valid.
        let read = unsafe { libc::read(r, &mut buf as *mut u8 as *mut _, 1) };
        assert_eq!(read, 1);
        assert_eq!(buf, 0xAB);
        close_fd(r).unwrap();
        close_fd(w).unwrap();
    }
}

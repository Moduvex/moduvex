//! Shared sockaddr conversion utilities for TCP and UDP.
//!
//! Eliminates duplication of `socketaddr_to_raw`, `reclaim_raw_sockaddr`, and
//! `sockaddr_to_socketaddr` across `tcp_listener`, `tcp_stream`, and `udp_socket`.

use std::io;
use std::net::SocketAddr;

/// Convert `SocketAddr` to a heap-allocated raw sockaddr pointer.
///
/// Caller MUST call [`reclaim_raw_sockaddr`] with the same `addr` after the syscall
/// to free the heap allocation.
pub(super) fn socketaddr_to_raw(addr: SocketAddr) -> (*const libc::sockaddr, libc::socklen_t) {
    match addr {
        SocketAddr::V4(v4) => {
            let octets = v4.ip().octets();
            // SAFETY: zeroed() is a valid bit pattern; all fields set below.
            let mut sin: libc::sockaddr_in = unsafe { std::mem::zeroed() };
            sin.sin_family = libc::AF_INET as libc::sa_family_t;
            sin.sin_port = v4.port().to_be();
            sin.sin_addr = libc::in_addr {
                s_addr: u32::from_be_bytes(octets).to_be(),
            };
            let ptr = Box::into_raw(Box::new(sin)) as *const libc::sockaddr;
            (
                ptr,
                std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
            )
        }
        SocketAddr::V6(v6) => {
            // SAFETY: zeroed() is a valid bit pattern; all fields set below.
            let mut sin6: libc::sockaddr_in6 = unsafe { std::mem::zeroed() };
            sin6.sin6_family = libc::AF_INET6 as libc::sa_family_t;
            sin6.sin6_port = v6.port().to_be();
            sin6.sin6_flowinfo = v6.flowinfo();
            sin6.sin6_addr = libc::in6_addr {
                s6_addr: v6.ip().octets(),
            };
            sin6.sin6_scope_id = v6.scope_id();
            let ptr = Box::into_raw(Box::new(sin6)) as *const libc::sockaddr;
            (
                ptr,
                std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t,
            )
        }
    }
}

/// # Safety
/// `ptr` must have been produced by [`socketaddr_to_raw`] with the matching `addr`.
pub(super) unsafe fn reclaim_raw_sockaddr(ptr: *const libc::sockaddr, addr: SocketAddr) {
    match addr {
        SocketAddr::V4(_) => drop(Box::from_raw(ptr as *mut libc::sockaddr_in)),
        SocketAddr::V6(_) => drop(Box::from_raw(ptr as *mut libc::sockaddr_in6)),
    }
}

/// Convert a kernel-filled `sockaddr_in6` buffer (may actually be `sockaddr_in`)
/// to a `SocketAddr`. The `sin6_family` field disambiguates the variant.
pub(super) fn sockaddr_to_socketaddr(
    addr: &libc::sockaddr_in6,
    len: libc::socklen_t,
) -> io::Result<SocketAddr> {
    let family = addr.sin6_family as libc::c_int;
    match family {
        libc::AF_INET if len >= std::mem::size_of::<libc::sockaddr_in>() as u32 => {
            // SAFETY: kernel wrote AF_INET data of the correct size; reinterpreting
            // the first sizeof(sockaddr_in) bytes as sockaddr_in is valid.
            let v4: &libc::sockaddr_in =
                unsafe { &*(addr as *const _ as *const libc::sockaddr_in) };
            let ip = std::net::Ipv4Addr::from(u32::from_be(v4.sin_addr.s_addr));
            let port = u16::from_be(v4.sin_port);
            Ok(SocketAddr::V4(std::net::SocketAddrV4::new(ip, port)))
        }
        libc::AF_INET6 if len >= std::mem::size_of::<libc::sockaddr_in6>() as u32 => {
            let ip = std::net::Ipv6Addr::from(addr.sin6_addr.s6_addr);
            let port = u16::from_be(addr.sin6_port);
            Ok(SocketAddr::V6(std::net::SocketAddrV6::new(
                ip,
                port,
                addr.sin6_flowinfo,
                addr.sin6_scope_id,
            )))
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported address family: {family}"),
        )),
    }
}

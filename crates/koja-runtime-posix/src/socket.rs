//! POSIX socket runtime functions.
//!
//! All sockets are created in non-blocking mode. When a syscall returns
//! `EAGAIN`, the calling process suspends via [`io_block`] until the
//! reactor detects readiness, then retries.

use std::ffi::c_char;
use std::io;
use std::mem;
use std::ptr;
use std::time::Instant;

use koja_runtime_core::{IoWait, deadline_from_user_millis};

use crate::ffi::{
    AF_INET, Addrinfo, EAGAIN, EINPROGRESS, EINTR, EINVAL, ENOTCONN, SO_ERROR, SO_REUSEADDR,
    SOL_SOCKET, SockaddrIn, get_errno, libc_accept, libc_bind, libc_connect, libc_freeaddrinfo,
    libc_getaddrinfo, libc_getpeername, libc_getsockopt, libc_listen, libc_recvfrom, libc_sendto,
    libc_setsockopt, libc_socket, set_nonblocking,
};
use crate::memory;
use crate::reactor::{Interest, block_until_ready, io_block};
use crate::util::{LastError, alloc_binary, set_last_error};

/// Accepts a connection on a listening socket. If no connection is
/// pending, suspends the process until one arrives or `timeout_ms`
/// passes (negative for no limit). Returns the new client fd (also set
/// to non-blocking), or -1 on error.
#[unsafe(no_mangle)]
pub extern "C" fn koja_socket_accept(fd: i32, timeout_ms: i64) -> i32 {
    let deadline = deadline_from_user_millis(timeout_ms);
    match block_until_ready(fd, Interest::Readable, deadline, || unsafe {
        libc_accept(fd, ptr::null_mut(), ptr::null_mut()) as isize
    }) {
        Ok(client) => {
            let client = client as i32;
            set_nonblocking(client);
            client
        }
        Err(e) => {
            set_last_error(e);
            -1
        }
    }
}

/// Binds a socket to a local IP address and port. Returns 0 on success,
/// -1 on error.
#[unsafe(no_mangle)]
pub extern "C" fn koja_socket_bind(fd: i32, ip_ptr: *const u8, ip_length: i64, port: i64) -> i64 {
    let Ok(ip_length) = usize::try_from(ip_length) else {
        set_last_error("invalid IP address length");
        return -1;
    };
    let (addr, addr_len) = match build_sockaddr_from_ip(ip_ptr, ip_length, port) {
        Ok(v) => v,
        Err(e) => {
            set_last_error(e);
            return -1;
        }
    };
    let ret = unsafe { libc_bind(fd, &addr as *const SockaddrIn as *const u8, addr_len) };
    if ret < 0 {
        set_last_error(io::Error::last_os_error());
        return -1;
    }
    0
}

/// Connects a socket to a remote IP address and port. For non-blocking
/// sockets, suspends the process until the TCP handshake completes or
/// `timeout_ms` passes (negative for no limit). Returns 0 on success,
/// -1 on error.
#[unsafe(no_mangle)]
pub extern "C" fn koja_socket_connect(
    fd: i32,
    ip_ptr: *const u8,
    ip_length: i64,
    port: i64,
    timeout_ms: i64,
) -> i64 {
    let deadline = deadline_from_user_millis(timeout_ms);
    let result = connect_start(fd, ip_ptr, ip_length, port).and_then(|progress| {
        if progress == ConnectProgress::Connected {
            return Ok(());
        }
        loop {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(io::ErrorKind::TimedOut.into());
            }
            let wait = io_block(fd, Interest::Writable, deadline);
            if wait == IoWait::Interrupted {
                return Err(io::Error::from_raw_os_error(EINTR));
            }
            if connect_finish(fd)? == ConnectProgress::Connected {
                return Ok(());
            }
            if wait == IoWait::TimedOut {
                return Err(io::ErrorKind::TimedOut.into());
            }
        }
    });
    match result {
        Ok(()) => 0,
        Err(e) => {
            set_last_error(e);
            -1
        }
    }
}

/// Where a non-blocking connect stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectProgress {
    Connected,
    InProgress,
}

/// Issues `connect(2)`. `InProgress` means the handshake continues in the
/// background and the caller waits for the fd to become writable, then
/// calls [`connect_finish`].
pub fn connect_start(
    fd: i32,
    ip_ptr: *const u8,
    ip_length: i64,
    port: i64,
) -> io::Result<ConnectProgress> {
    let Ok(ip_length) = usize::try_from(ip_length) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid IP address length",
        ));
    };
    let (addr, addr_len) = build_sockaddr_from_ip(ip_ptr, ip_length, port)?;
    let ret = unsafe { libc_connect(fd, &addr as *const SockaddrIn as *const u8, addr_len) };
    if ret == 0 {
        return Ok(ConnectProgress::Connected);
    }
    let errno = get_errno();
    if errno == EINPROGRESS || errno == EAGAIN {
        return Ok(ConnectProgress::InProgress);
    }
    Err(io::Error::last_os_error())
}

/// Checks a connect that was `InProgress` after a writability wake. A
/// peer name means the handshake finished. Otherwise `SO_ERROR` holds the
/// failure, or zero while the handshake is still running (a timer wake).
/// macOS reports a failed connect from `getpeername` as `EINVAL`.
pub fn connect_finish(fd: i32) -> io::Result<ConnectProgress> {
    let mut addr: SockaddrIn = unsafe { mem::zeroed() };
    let mut addr_len = mem::size_of::<SockaddrIn>() as u32;
    let ret =
        unsafe { libc_getpeername(fd, &mut addr as *mut SockaddrIn as *mut u8, &mut addr_len) };
    if ret == 0 {
        return Ok(ConnectProgress::Connected);
    }
    let errno = get_errno();
    if errno != ENOTCONN && errno != EINVAL {
        return Err(io::Error::last_os_error());
    }

    let mut err: i32 = 0;
    let mut len = mem::size_of::<i32>() as u32;
    let ret = unsafe {
        libc_getsockopt(
            fd,
            SOL_SOCKET,
            SO_ERROR,
            &mut err as *mut i32 as *mut u8,
            &mut len,
        )
    };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    if err != 0 {
        return Err(io::Error::from_raw_os_error(err));
    }
    Ok(ConnectProgress::InProgress)
}

/// Creates a new socket in non-blocking mode. `sock_type` is the POSIX
/// socket type constant (e.g. `SOCK_STREAM`, `SOCK_DGRAM`), resolved by
/// the codegen from the Koja `SocketKind` enum.
/// Returns the fd on success, -1 on error.
#[unsafe(no_mangle)]
pub extern "C" fn koja_socket_create(sock_type: i64) -> i32 {
    let fd = unsafe { libc_socket(AF_INET, sock_type as i32, 0) };
    if fd < 0 {
        set_last_error(io::Error::last_os_error());
        return -1;
    }
    set_nonblocking(fd);
    fd
}

/// Marks a socket as listening for incoming connections with the given
/// backlog depth. Returns 0 on success, -1 on error.
#[unsafe(no_mangle)]
pub extern "C" fn koja_socket_listen(fd: i32, backlog: i64) -> i64 {
    let ret = unsafe { libc_listen(fd, backlog as i32) };
    if ret < 0 {
        set_last_error(io::Error::last_os_error());
        return -1;
    }
    0
}

/// Receives data from a socket with sender address information. Suspends
/// the process if no data is available. Returns a pointer to a
/// `(data, ip_binary, port)` triple, or null on error.
///
/// # Safety
/// `fd` must be a valid open socket file descriptor.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_socket_recv_from(fd: i32, count: i64) -> *mut u8 {
    let mut buf = vec![0u8; count as usize];
    let mut sender_addr: SockaddrIn = unsafe { mem::zeroed() };
    let mut addr_len = mem::size_of::<SockaddrIn>() as u32;

    let n = match block_until_ready(fd, Interest::Readable, None, || unsafe {
        libc_recvfrom(
            fd,
            buf.as_mut_ptr(),
            count as usize,
            0,
            &mut sender_addr as *mut SockaddrIn as *mut u8,
            &mut addr_len,
        )
    }) {
        Ok(n) => n,
        Err(e) => {
            set_last_error(e);
            return ptr::null_mut();
        }
    };

    buf.truncate(n as usize);
    let data = alloc_binary(&buf);
    unsafe {
        let ip_bytes = sender_addr.sin_addr.to_ne_bytes();
        let ip_bin = alloc_binary(&ip_bytes);
        let sender_port = u16::from_be(sender_addr.sin_port) as i64;

        let result_size = 3 * mem::size_of::<*mut u8>();
        let result = memory::alloc(result_size);
        *(result as *mut *mut u8) = data;
        *((result as *mut *mut u8).add(1)) = ip_bin;
        *((result as *mut i64).add(2)) = sender_port;
        result
    }
}

/// Resolves a hostname to a list of IPv4 addresses via `getaddrinfo`.
/// This call blocks the worker thread (DNS is not fd-based).
/// Returns a pointer to a length-prefixed array of Binary pointers, or null on error.
///
/// # Safety
/// `hostname` must be a valid null-terminated C string pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_socket_resolve(hostname: *const u8) -> *mut u8 {
    let mut result: *mut Addrinfo = ptr::null_mut();

    let ret = unsafe {
        libc_getaddrinfo(
            hostname as *const c_char,
            ptr::null(),
            ptr::null(),
            &mut result,
        )
    };
    if ret != 0 {
        set_last_error(LastError::name_not_found("getaddrinfo failed"));
        return ptr::null_mut();
    }

    let mut addrs: Vec<*mut u8> = Vec::new();
    let mut cur = result;
    while !cur.is_null() {
        let info = unsafe { &*cur };
        if info.ai_family == AF_INET && info.ai_addrlen as usize >= mem::size_of::<SockaddrIn>() {
            let sa = info.ai_addr as *const SockaddrIn;
            let ip_bytes = unsafe { (*sa).sin_addr.to_ne_bytes() };
            let bin = alloc_binary(&ip_bytes);
            addrs.push(bin);
        }
        cur = info.ai_next;
    }
    unsafe { libc_freeaddrinfo(result) };

    if addrs.is_empty() {
        set_last_error(LastError::name_not_found("no addresses found"));
        return ptr::null_mut();
    }

    let buf_size = 8 + addrs.len() * mem::size_of::<*mut u8>();
    let buf = memory::alloc(buf_size);
    unsafe {
        *(buf as *mut i64) = addrs.len() as i64;
        let ptrs = buf.add(8) as *mut *mut u8;
        for (i, p) in addrs.iter().enumerate() {
            *ptrs.add(i) = *p;
        }
    }
    buf
}

/// Sends data to a remote address via a socket. Suspends the process
/// if the send buffer is full. Returns bytes sent, or -1 on error.
///
/// # Safety
/// Both pointers must contain the corresponding number of readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_socket_send_to(
    fd: i32,
    data_ptr: *const u8,
    data_length: i64,
    ip_ptr: *const u8,
    ip_length: i64,
    port: i64,
) -> i64 {
    let (Ok(data_length), Ok(ip_length)) =
        (usize::try_from(data_length), usize::try_from(ip_length))
    else {
        set_last_error("invalid socket buffer length");
        return -1;
    };

    let (addr, addr_len) = match build_sockaddr_from_ip(ip_ptr, ip_length, port) {
        Ok(v) => v,
        Err(e) => {
            set_last_error(e);
            return -1;
        }
    };

    match block_until_ready(fd, Interest::Writable, None, || unsafe {
        libc_sendto(
            fd,
            data_ptr,
            data_length,
            0,
            &addr as *const SockaddrIn as *const u8,
            addr_len,
        )
    }) {
        Ok(sent) => sent as i64,
        Err(e) => {
            set_last_error(e);
            -1
        }
    }
}

/// Enables `SO_REUSEADDR` on a socket. Returns 0 on success, -1 on error.
#[unsafe(no_mangle)]
pub extern "C" fn koja_socket_setsockopt_reuse(fd: i32) -> i64 {
    let optval: i32 = 1;
    let ret = unsafe {
        libc_setsockopt(
            fd,
            SOL_SOCKET,
            SO_REUSEADDR,
            &optval as *const i32 as *const u8,
            mem::size_of::<i32>() as u32,
        )
    };
    if ret < 0 {
        set_last_error(io::Error::last_os_error());
        return -1;
    }
    0
}

/// Constructs a `SockaddrIn` from an IPv4 address and port number.
fn build_sockaddr_from_ip(
    ip_ptr: *const u8,
    ip_length: usize,
    port: i64,
) -> Result<(SockaddrIn, u32), io::Error> {
    match ip_length {
        4 => {
            let mut ip_bytes = [0u8; 4];
            unsafe { ptr::copy_nonoverlapping(ip_ptr, ip_bytes.as_mut_ptr(), 4) };
            let addr = SockaddrIn {
                #[cfg(target_os = "macos")]
                sin_len: mem::size_of::<SockaddrIn>() as u8,
                #[cfg(target_os = "macos")]
                sin_family: AF_INET as u8,
                #[cfg(target_os = "linux")]
                sin_family: AF_INET as u16,
                sin_port: (port as u16).to_be(),
                sin_addr: u32::from_ne_bytes(ip_bytes),
                sin_zero: [0; 8],
            };
            Ok((addr, mem::size_of::<SockaddrIn>() as u32))
        }
        _ => Err(io::Error::other("unsupported address length")),
    }
}

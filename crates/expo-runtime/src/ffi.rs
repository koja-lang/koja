//! Raw libc and platform FFI declarations used by the runtime.

/// IPv4 address family.
pub const AF_INET: i32 = 2;
/// Stream (TCP) socket type.
pub const SOCK_STREAM: i32 = 1;
/// Datagram (UDP) socket type.
pub const SOCK_DGRAM: i32 = 2;

#[cfg(target_os = "macos")]
mod platform {
    pub const SOL_SOCKET: i32 = 0xFFFF;
    pub const SO_REUSEADDR: i32 = 0x0004;
    pub const SO_ERROR: i32 = 0x1007;
    pub const O_NONBLOCK: i32 = 0x0004;
    pub const EAGAIN: i32 = 35;
    pub const EINPROGRESS: i32 = 36;
}

#[cfg(target_os = "linux")]
mod platform {
    pub const SOL_SOCKET: i32 = 1;
    pub const SO_REUSEADDR: i32 = 2;
    pub const SO_ERROR: i32 = 4;
    pub const O_NONBLOCK: i32 = 0x800;
    pub const EAGAIN: i32 = 11;
    pub const EINPROGRESS: i32 = 115;
}

pub use platform::*;

/// Get file descriptor flags.
pub const F_GETFL: i32 = 3;
/// Set file descriptor flags.
pub const F_SETFL: i32 = 4;

/// BSD `sockaddr_in` (macOS: includes `sin_len` field).
#[cfg(target_os = "macos")]
#[repr(C)]
pub struct SockaddrIn {
    pub sin_len: u8,
    pub sin_family: u8,
    pub sin_port: u16,
    pub sin_addr: u32,
    pub sin_zero: [u8; 8],
}

/// Linux `sockaddr_in` (no `sin_len` field, `sin_family` is `u16`).
#[cfg(target_os = "linux")]
#[repr(C)]
pub struct SockaddrIn {
    pub sin_family: u16,
    pub sin_port: u16,
    pub sin_addr: u32,
    pub sin_zero: [u8; 8],
}

/// POSIX `addrinfo` returned by `getaddrinfo`.
///
/// Field order differs between macOS and Linux:
/// - macOS: canonname, addr, next
/// - Linux: addr, canonname, next
#[cfg(target_os = "macos")]
#[repr(C)]
pub struct Addrinfo {
    pub ai_flags: i32,
    pub ai_family: i32,
    pub ai_socktype: i32,
    pub ai_protocol: i32,
    pub ai_addrlen: u32,
    pub ai_canonname: *mut u8,
    pub ai_addr: *mut u8,
    pub ai_next: *mut Addrinfo,
}

#[cfg(target_os = "linux")]
#[repr(C)]
pub struct Addrinfo {
    pub ai_flags: i32,
    pub ai_family: i32,
    pub ai_socktype: i32,
    pub ai_protocol: i32,
    pub ai_addrlen: u32,
    pub ai_addr: *mut u8,
    pub ai_canonname: *mut u8,
    pub ai_next: *mut Addrinfo,
}

unsafe extern "C" {
    pub fn expo_context_switch(save_sp: *mut *mut u8, load_sp: *mut u8);

    pub fn fflush(stream: *mut u8) -> i32;
    pub fn setvbuf(stream: *mut u8, buf: *mut u8, mode: i32, size: usize) -> i32;
    pub fn malloc(size: usize) -> *mut u8;

    #[link_name = "accept"]
    pub fn libc_accept(fd: i32, addr: *mut u8, addrlen: *mut u32) -> i32;
    #[link_name = "bind"]
    pub fn libc_bind(fd: i32, addr: *const u8, addrlen: u32) -> i32;
    #[link_name = "close"]
    pub fn libc_close(fd: i32) -> i32;
    #[link_name = "connect"]
    pub fn libc_connect(fd: i32, addr: *const u8, addrlen: u32) -> i32;
    #[link_name = "freeaddrinfo"]
    pub fn libc_freeaddrinfo(res: *mut Addrinfo);
    #[link_name = "getaddrinfo"]
    pub fn libc_getaddrinfo(
        node: *const i8,
        service: *const i8,
        hints: *const Addrinfo,
        res: *mut *mut Addrinfo,
    ) -> i32;
    #[link_name = "gethostname"]
    pub fn libc_gethostname(name: *mut i8, len: usize) -> i32;
    #[link_name = "listen"]
    pub fn libc_listen(fd: i32, backlog: i32) -> i32;
    #[link_name = "read"]
    pub fn libc_read(fd: i32, buf: *mut u8, count: usize) -> isize;
    #[link_name = "recvfrom"]
    pub fn libc_recvfrom(
        fd: i32,
        buf: *mut u8,
        len: usize,
        flags: i32,
        addr: *mut u8,
        addrlen: *mut u32,
    ) -> isize;
    #[link_name = "sendto"]
    pub fn libc_sendto(
        fd: i32,
        buf: *const u8,
        len: usize,
        flags: i32,
        addr: *const u8,
        addrlen: u32,
    ) -> isize;
    #[link_name = "setsockopt"]
    pub fn libc_setsockopt(
        fd: i32,
        level: i32,
        optname: i32,
        optval: *const u8,
        optlen: u32,
    ) -> i32;
    #[link_name = "socket"]
    pub fn libc_socket(domain: i32, sock_type: i32, protocol: i32) -> i32;
    #[link_name = "write"]
    pub fn libc_write(fd: i32, buf: *const u8, count: usize) -> isize;
    #[link_name = "fcntl"]
    pub fn libc_fcntl(fd: i32, cmd: i32, ...) -> i32;
    #[cfg(target_os = "macos")]
    #[link_name = "getentropy"]
    pub fn libc_getentropy(buf: *mut u8, buflen: usize) -> i32;

    #[cfg(target_os = "linux")]
    #[link_name = "getrandom"]
    pub fn libc_getrandom(buf: *mut u8, buflen: usize, flags: u32) -> isize;

    #[link_name = "getsockopt"]
    pub fn libc_getsockopt(
        fd: i32,
        level: i32,
        optname: i32,
        optval: *mut u8,
        optlen: *mut u32,
    ) -> i32;
}

/// Returns the current `errno` value for this thread.
pub fn get_errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// Sets a file descriptor to non-blocking mode via `fcntl`.
pub fn set_nonblocking(fd: i32) {
    unsafe {
        let flags = libc_fcntl(fd, F_GETFL);
        if flags >= 0 {
            libc_fcntl(fd, F_SETFL, flags | O_NONBLOCK);
        }
    }
}

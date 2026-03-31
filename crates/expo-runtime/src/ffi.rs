//! Raw libc and platform FFI declarations used by the runtime.

/// BSD/POSIX `sockaddr_in` for IPv4 socket addresses.
#[repr(C)]
pub struct SockaddrIn {
    pub sin_len: u8,
    pub sin_family: u8,
    pub sin_port: u16,
    pub sin_addr: u32,
    pub sin_zero: [u8; 8],
}

/// POSIX `addrinfo` returned by `getaddrinfo`.
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

unsafe extern "C" {
    pub fn expo_context_switch(save_sp: *mut *mut u8, load_sp: *mut u8);

    pub fn fflush(stream: *mut u8) -> i32;
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
}

//! POSIX socket runtime functions.

use crate::ffi::{
    Addrinfo, SockaddrIn, libc_accept, libc_bind, libc_connect, libc_freeaddrinfo,
    libc_getaddrinfo, libc_listen, libc_recvfrom, libc_sendto, libc_setsockopt, libc_socket,
    malloc,
};
use crate::util::{alloc_binary, set_last_error};

/// Accepts a connection on a listening socket. Returns the new client fd,
/// or -1 on error.
#[unsafe(no_mangle)]
pub extern "C" fn expo_socket_accept(fd: i64) -> i64 {
    let client = unsafe { libc_accept(fd as i32, std::ptr::null_mut(), std::ptr::null_mut()) };
    if client < 0 {
        set_last_error(std::io::Error::last_os_error());
        return -1;
    }
    client as i64
}

/// Binds a socket to a local IP address and port. Returns 0 on success,
/// -1 on error.
#[unsafe(no_mangle)]
pub extern "C" fn expo_socket_bind(fd: i64, ip_ptr: *const u8, port: i64) -> i64 {
    let (addr, addr_len) = match build_sockaddr_from_ip(ip_ptr, port) {
        Ok(v) => v,
        Err(e) => {
            set_last_error(e);
            return -1;
        }
    };
    let ret = unsafe { libc_bind(fd as i32, &addr as *const SockaddrIn as *const u8, addr_len) };
    if ret < 0 {
        set_last_error(std::io::Error::last_os_error());
        return -1;
    }
    0
}

/// Connects a socket to a remote IP address and port. Returns 0 on success,
/// -1 on error.
#[unsafe(no_mangle)]
pub extern "C" fn expo_socket_connect(fd: i64, ip_ptr: *const u8, port: i64) -> i64 {
    let (addr, addr_len) = match build_sockaddr_from_ip(ip_ptr, port) {
        Ok(v) => v,
        Err(e) => {
            set_last_error(e);
            return -1;
        }
    };
    let ret = unsafe { libc_connect(fd as i32, &addr as *const SockaddrIn as *const u8, addr_len) };
    if ret < 0 {
        set_last_error(std::io::Error::last_os_error());
        return -1;
    }
    0
}

/// Creates a new socket. `kind`: 0 = stream (TCP), 1 = datagram (UDP).
/// Returns the fd on success, -1 on error.
#[unsafe(no_mangle)]
pub extern "C" fn expo_socket_create(kind: i64) -> i64 {
    let sock_type = match kind {
        0 => 1, // Stream -> SOCK_STREAM
        1 => 2, // Datagram -> SOCK_DGRAM
        _ => {
            set_last_error(std::io::Error::other("invalid socket kind"));
            return -1;
        }
    };
    let fd = unsafe {
        libc_socket(2 /* AF_INET */, sock_type, 0)
    };
    if fd < 0 {
        set_last_error(std::io::Error::last_os_error());
        return -1;
    }
    fd as i64
}

/// Marks a socket as listening for incoming connections with the given
/// backlog depth. Returns 0 on success, -1 on error.
#[unsafe(no_mangle)]
pub extern "C" fn expo_socket_listen(fd: i64, backlog: i64) -> i64 {
    let ret = unsafe { libc_listen(fd as i32, backlog as i32) };
    if ret < 0 {
        set_last_error(std::io::Error::last_os_error());
        return -1;
    }
    0
}

/// Receives data from a socket with sender address information.
/// Returns a pointer to a `(data, ip_binary, port)` triple, or null on error.
///
/// # Safety
/// `fd` must be a valid open socket file descriptor.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn expo_socket_recv_from(fd: i64, count: i64) -> *mut u8 {
    let mut buf = vec![0u8; count as usize];
    let mut sender_addr: SockaddrIn = unsafe { std::mem::zeroed() };
    let mut addr_len = std::mem::size_of::<SockaddrIn>() as u32;

    let n = unsafe {
        libc_recvfrom(
            fd as i32,
            buf.as_mut_ptr(),
            count as usize,
            0,
            &mut sender_addr as *mut SockaddrIn as *mut u8,
            &mut addr_len,
        )
    };
    if n < 0 {
        set_last_error(std::io::Error::last_os_error());
        return std::ptr::null_mut();
    }

    let data_len = n as usize;
    let str_alloc = 8 + data_len + 1;
    let str_base = unsafe { malloc(str_alloc) };
    unsafe {
        *(str_base as *mut i64) = (data_len as i64) * 8;
        let str_payload = str_base.add(8);
        std::ptr::copy_nonoverlapping(buf.as_ptr(), str_payload, data_len);
        *str_payload.add(data_len) = 0;

        let ip_bytes = sender_addr.sin_addr.to_ne_bytes();
        let ip_bin = alloc_binary(&ip_bytes);
        let sender_port = u16::from_be(sender_addr.sin_port) as i64;

        let result_size = 3 * std::mem::size_of::<*mut u8>();
        let result = malloc(result_size);
        *(result as *mut *mut u8) = str_payload;
        *((result as *mut *mut u8).add(1)) = ip_bin;
        *((result as *mut i64).add(2)) = sender_port;
        result
    }
}

/// Resolves a hostname to a list of IPv4 addresses via `getaddrinfo`.
/// Returns a pointer to a length-prefixed array of Binary pointers, or null on error.
///
/// # Safety
/// `hostname` must be a valid null-terminated C string pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn expo_socket_resolve(hostname: *const u8) -> *mut u8 {
    let mut result: *mut Addrinfo = std::ptr::null_mut();

    let ret = unsafe {
        libc_getaddrinfo(
            hostname as *const i8,
            std::ptr::null(),
            std::ptr::null(),
            &mut result,
        )
    };
    if ret != 0 {
        set_last_error(std::io::Error::other("getaddrinfo failed"));
        return std::ptr::null_mut();
    }

    let mut addrs: Vec<*mut u8> = Vec::new();
    let mut cur = result;
    while !cur.is_null() {
        let info = unsafe { &*cur };
        if info.ai_family == 2 && info.ai_addrlen as usize >= std::mem::size_of::<SockaddrIn>() {
            let sa = info.ai_addr as *const SockaddrIn;
            let ip_bytes = unsafe { (*sa).sin_addr.to_ne_bytes() };
            let bin = alloc_binary(&ip_bytes);
            addrs.push(bin);
        }
        cur = info.ai_next;
    }
    unsafe { libc_freeaddrinfo(result) };

    if addrs.is_empty() {
        set_last_error(std::io::Error::other("no addresses found"));
        return std::ptr::null_mut();
    }

    let buf_size = 8 + addrs.len() * std::mem::size_of::<*mut u8>();
    let buf = unsafe { malloc(buf_size) };
    unsafe {
        *(buf as *mut i64) = addrs.len() as i64;
        let ptrs = buf.add(8) as *mut *mut u8;
        for (i, p) in addrs.iter().enumerate() {
            *ptrs.add(i) = *p;
        }
    }
    buf
}

/// Sends data to a remote address via a socket. Returns bytes sent, or -1 on error.
///
/// # Safety
/// `data_ptr` must be a valid null-terminated string. `ip_ptr` must point to the payload
/// of a valid Binary allocation (4 or 16 bytes) with an 8-byte length header at offset -8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn expo_socket_send_to(
    fd: i64,
    data_ptr: *const u8,
    ip_ptr: *const u8,
    port: i64,
) -> i64 {
    let data_len = unsafe {
        let mut p = data_ptr;
        while *p != 0 {
            p = p.offset(1);
        }
        p.offset_from(data_ptr) as usize
    };

    let (addr, addr_len) = match build_sockaddr_from_ip(ip_ptr, port) {
        Ok(v) => v,
        Err(e) => {
            set_last_error(e);
            return -1;
        }
    };

    let sent = unsafe {
        libc_sendto(
            fd as i32,
            data_ptr,
            data_len,
            0,
            &addr as *const SockaddrIn as *const u8,
            addr_len,
        )
    };
    if sent < 0 {
        set_last_error(std::io::Error::last_os_error());
        return -1;
    }
    sent as i64
}

/// Enables `SO_REUSEADDR` on a socket. Returns 0 on success, -1 on error.
#[unsafe(no_mangle)]
pub extern "C" fn expo_socket_setsockopt_reuse(fd: i64) -> i64 {
    let optval: i32 = 1;
    let ret = unsafe {
        libc_setsockopt(
            fd as i32,
            0xFFFF, // SOL_SOCKET
            0x0004, // SO_REUSEADDR
            &optval as *const i32 as *const u8,
            std::mem::size_of::<i32>() as u32,
        )
    };
    if ret < 0 {
        set_last_error(std::io::Error::last_os_error());
        return -1;
    }
    0
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Constructs a `SockaddrIn` from a Binary-encoded IPv4 address and port number.
fn build_sockaddr_from_ip(
    ip_ptr: *const u8,
    port: i64,
) -> Result<(SockaddrIn, u32), std::io::Error> {
    let bit_len = unsafe { *(ip_ptr.offset(-8) as *const i64) };
    let byte_len = (bit_len >> 3) as usize;
    match byte_len {
        4 => {
            let mut ip_bytes = [0u8; 4];
            unsafe { std::ptr::copy_nonoverlapping(ip_ptr, ip_bytes.as_mut_ptr(), 4) };
            let addr = SockaddrIn {
                sin_len: std::mem::size_of::<SockaddrIn>() as u8,
                sin_family: 2, // AF_INET
                sin_port: (port as u16).to_be(),
                sin_addr: u32::from_ne_bytes(ip_bytes),
                sin_zero: [0; 8],
            };
            Ok((addr, std::mem::size_of::<SockaddrIn>() as u32))
        }
        _ => Err(std::io::Error::other("unsupported address length")),
    }
}

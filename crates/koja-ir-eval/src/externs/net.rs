//! Externs declared in `lib/net/src/net.koja` and
//! `lib/net/src/error.koja`.
//!
//! Eval reuses the runtime's `koja_socket_*` symbols (sockaddr
//! building, last-error recording) but normalizes every fd it hands
//! out to **blocking** mode. The runtime targets the coroutine
//! scheduler: it creates non-blocking fds and parks the calling
//! process via the reactor on `EAGAIN` — machinery that doesn't
//! exist under eval. On a blocking fd no syscall ever returns
//! `EAGAIN`, so the runtime's `block_until_ready` loops complete on
//! their first iteration and the reactor path is never taken —
//! fd-blocking ops just block the (single) interpreter thread.
//!
//! Concretely:
//!
//! - `create` / `accept` / `try_accept` call the runtime symbol,
//!   then clear `O_NONBLOCK` on the returned fd.
//! - `try_accept` additionally pre-polls the listener — the runtime
//!   symbol relies on a non-blocking listener to deliver its
//!   "nothing pending" `-2`, which a blocking listener can't do.
//! - `bind` / `connect` / `listen` / `send_to` / `setsockopt_reuse`
//!   and the last-error readers pass straight through.

use crate::error::RuntimeError;
use crate::externs::marshal::{pass_through_externs, type_mismatch};
use crate::value::Value;

/// `fcntl` get-flags command. API contract: MUST equal
/// [`koja_runtime`]'s `ffi::F_GETFL`.
const F_GETFL: i32 = 3;
/// `fcntl` set-flags command. API contract: MUST equal
/// [`koja_runtime`]'s `ffi::F_SETFL`.
const F_SETFL: i32 = 4;
/// Non-blocking fd status flag. API contract: MUST equal
/// [`koja_runtime`]'s `ffi::O_NONBLOCK` for the target OS.
#[cfg(target_os = "macos")]
const O_NONBLOCK: i32 = 0x0004;
#[cfg(target_os = "linux")]
const O_NONBLOCK: i32 = 0x800;

/// `poll(2)` readability event bit (same value on macOS and Linux).
const POLLIN: i16 = 0x1;

/// POSIX `struct pollfd` (identical layout on macOS and Linux).
#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

unsafe extern "C" {
    fn fcntl(fd: i32, cmd: i32, ...) -> i32;
    fn koja_socket_accept(fd: i32) -> i32;
    fn koja_socket_create(sock_type: i64) -> i32;
    fn koja_socket_try_accept(fd: i32) -> i32;
    fn poll(fds: *mut PollFd, nfds: u32, timeout: i32) -> i32;
}

pass_through_externs! {
    errno_code => fn koja_errno_code() -> Int32;
    last_error => fn koja_last_error() -> CPtr;
    last_error_code => fn koja_last_error_code() -> Int32;
    socket_bind => fn koja_socket_bind(fd: Int32, ip: CPtr, port: Int64) -> Int64;
    socket_connect => fn koja_socket_connect(fd: Int32, ip: CPtr, port: Int64) -> Int64;
    socket_listen => fn koja_socket_listen(fd: Int32, backlog: Int64) -> Int64;
    socket_send_to => fn koja_socket_send_to(fd: Int32, data: CPtr, ip: CPtr, port: Int64) -> Int64;
    socket_setsockopt_reuse => fn koja_socket_setsockopt_reuse(fd: Int32) -> Int64;
}

pub(super) fn socket_accept(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Int(fd)] = args else {
        return Err(type_mismatch("koja_socket_accept", "(fd: Int32)", args));
    };
    let client = unsafe { koja_socket_accept(*fd as i32) };
    if client >= 0 {
        set_blocking(client);
    }
    Ok(Value::Int(client as i64))
}

pub(super) fn socket_create(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Int(sock_type)] = args else {
        return Err(type_mismatch("koja_socket_create", "(kind: Int64)", args));
    };
    let fd = unsafe { koja_socket_create(*sock_type) };
    if fd >= 0 {
        set_blocking(fd);
    }
    Ok(Value::Int(fd as i64))
}

pub(super) fn socket_try_accept(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Int(fd)] = args else {
        return Err(type_mismatch("koja_socket_try_accept", "(fd: Int32)", args));
    };
    let fd = *fd as i32;
    if !readable_now(fd) {
        return Ok(Value::Int(-2));
    }
    let client = unsafe { koja_socket_try_accept(fd) };
    if client >= 0 {
        set_blocking(client);
    }
    Ok(Value::Int(client as i64))
}

/// Clear `O_NONBLOCK` on `fd`. Inverse of the runtime's
/// `set_nonblocking`; applied to every fd a runtime socket symbol
/// hands back so subsequent reads/writes block instead of `EAGAIN`ing
/// into the (absent) reactor.
fn set_blocking(fd: i32) {
    unsafe {
        let flags = fcntl(fd, F_GETFL);
        if flags >= 0 {
            fcntl(fd, F_SETFL, flags & !O_NONBLOCK);
        }
    }
}

/// Zero-timeout `poll` for pending readability. Guards `try_accept`:
/// a blocking listener would make the runtime's bare `accept` wait
/// for a connection instead of reporting "nothing pending".
fn readable_now(fd: i32) -> bool {
    let mut pollfd = PollFd {
        fd,
        events: POLLIN,
        revents: 0,
    };
    let ready = unsafe { poll(&mut pollfd, 1, 0) };
    ready > 0 && (pollfd.revents & POLLIN) != 0
}

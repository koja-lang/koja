//! Externs declared in `lib/net/src/net.koja` and
//! `lib/net/src/error.koja`.
//!
//! Eval reuses the runtime's `koja_socket_*` symbols (sockaddr building,
//! last-error recording) over **non-blocking** fds: the runtime creates
//! them non-blocking and eval keeps them that way. Because the native
//! blocking entry points (`accept` / `send_to`) park via the *native*
//! reactor on `EAGAIN`, which eval cannot drive, eval waits for readiness
//! through its own [`crate::reactor`] *first* and only then delegates, so
//! the native syscall succeeds on its first try:
//!
//! - `accept` / `send_to` [`io_block`](crate::reactor::io_block) for
//!   readiness, then call the native symbol.
//! - `try_accept` calls the native non-blocking symbol directly (a
//!   non-blocking listener reports its `-2` "nothing pending" itself).
//! - `connect` is the one path that cannot pre-wait (the fd is not
//!   writable until the handshake is initiated), so it flips the fd to
//!   blocking for the duration of the native call (a bounded, one-time
//!   setup wait), then restores non-blocking for subsequent I/O.
//! - `create` / `bind` / `listen` / `setsockopt_reuse` and the last-error
//!   readers pass straight through.

use koja_runtime_core::Interest;

use crate::error::RuntimeError;
use crate::externs::marshal::{pass_through_externs, type_mismatch};
use crate::reactor;
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

unsafe extern "C" {
    fn fcntl(fd: i32, cmd: i32, ...) -> i32;
    fn koja_socket_accept(fd: i32) -> i32;
    fn koja_socket_connect(fd: i32, ip: *const u8, port: i64) -> i64;
    fn koja_socket_create(sock_type: i64) -> i32;
    fn koja_socket_send_to(fd: i32, data: *const u8, ip: *const u8, port: i64) -> i64;
    fn koja_socket_try_accept(fd: i32) -> i32;
}

pass_through_externs! {
    errno_code => fn koja_errno_code() -> Int32;
    last_error => fn koja_last_error() -> CPtr;
    last_error_code => fn koja_last_error_code() -> Int32;
    socket_bind => fn koja_socket_bind(fd: Int32, ip: CPtr, port: Int64) -> Int64;
    socket_listen => fn koja_socket_listen(fd: Int32, backlog: Int64) -> Int64;
    socket_setsockopt_reuse => fn koja_socket_setsockopt_reuse(fd: Int32) -> Int64;
}

/// `koja_socket_create(kind)`: a fresh non-blocking socket (the native
/// symbol already sets `O_NONBLOCK`, eval keeps it).
pub(super) fn socket_create(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Int(sock_type)] = args else {
        return Err(type_mismatch("koja_socket_create", "(kind: Int64)", args));
    };
    let fd = unsafe { koja_socket_create(*sock_type) };
    Ok(Value::Int(i64::from(fd)))
}

/// `koja_socket_accept(fd)`: wait for the listener to be readable (a
/// pending connection), then delegate to the native blocking accept, which
/// now completes on its first syscall.
pub(super) async fn socket_accept(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Int(fd)] = args else {
        return Err(type_mismatch("koja_socket_accept", "(fd: Int32)", args));
    };
    // Interrupted by a signal, not readiness: return the native -1 sentinel.
    if reactor::io_block(*fd as i32, Interest::Readable).await {
        return Ok(Value::Int(i64::from(-1i32)));
    }
    let client = unsafe { koja_socket_accept(*fd as i32) };
    Ok(Value::Int(i64::from(client)))
}

/// `koja_socket_try_accept(fd)`: native non-blocking accept. Returns the
/// client fd, `-2` when nothing is pending, or `-1` on error.
pub(super) fn socket_try_accept(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Int(fd)] = args else {
        return Err(type_mismatch("koja_socket_try_accept", "(fd: Int32)", args));
    };
    let client = unsafe { koja_socket_try_accept(*fd as i32) };
    Ok(Value::Int(i64::from(client)))
}

/// `koja_socket_connect(fd, ip, port)`: the one path that cannot pre-wait
/// for readiness (the fd is not writable until the handshake starts).
/// Flip the fd to blocking so the native connect waits in the kernel
/// instead of parking via the native reactor, then restore non-blocking
/// for subsequent reads / writes.
pub(super) fn socket_connect(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Int(fd), Value::CPtr(ip), Value::Int(port)] = args else {
        return Err(type_mismatch(
            "koja_socket_connect",
            "(fd: Int32, ip: CPtr, port: Int64)",
            args,
        ));
    };
    let fd = *fd as i32;
    set_nonblocking(fd, false);
    let result = unsafe { koja_socket_connect(fd, *ip, *port) };
    set_nonblocking(fd, true);
    Ok(Value::Int(result))
}

/// `koja_socket_send_to(fd, data, ip, port)`: wait for the socket to be
/// writable, then delegate to the native sender.
pub(super) async fn socket_send_to(args: &[Value]) -> Result<Value, RuntimeError> {
    let [
        Value::Int(fd),
        Value::CPtr(data),
        Value::CPtr(ip),
        Value::Int(port),
    ] = args
    else {
        return Err(type_mismatch(
            "koja_socket_send_to",
            "(fd: Int32, data: CPtr, ip: CPtr, port: Int64)",
            args,
        ));
    };
    // Interrupted by a signal: return the native -1 sentinel.
    if reactor::io_block(*fd as i32, Interest::Writable).await {
        return Ok(Value::Int(-1));
    }
    let sent = unsafe { koja_socket_send_to(*fd as i32, *data, *ip, *port) };
    Ok(Value::Int(sent))
}

/// Set or clear `O_NONBLOCK` on `fd`. A no-op on `fcntl` failure (the
/// subsequent syscall surfaces any real error).
fn set_nonblocking(fd: i32, nonblocking: bool) {
    unsafe {
        let flags = fcntl(fd, F_GETFL);
        if flags < 0 {
            return;
        }
        let updated = if nonblocking {
            flags | O_NONBLOCK
        } else {
            flags & !O_NONBLOCK
        };
        fcntl(fd, F_SETFL, updated);
    }
}

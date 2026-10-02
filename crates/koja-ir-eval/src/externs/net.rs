//! Externs declared in `lib/net/src/net.koja` and
//! `lib/net/src/error.koja`.
//!
//! Eval reuses the runtime's `koja_socket_*` symbols over the
//! non-blocking fds the runtime creates, waiting for readiness through
//! [`crate::reactor`] before each native call (see there for why).
//!
//! - `accept` / `send_to` [`io_block`](crate::reactor::io_block) for
//!   readiness, then call the native symbol.
//! - `try_accept` calls the native non-blocking symbol directly (a
//!   non-blocking listener reports its `-2` "nothing pending" itself).
//! - `connect` cannot pre-wait (the fd is not writable until the
//!   handshake is initiated), so it drives the runtime's split
//!   `connect_start` / `connect_finish` around an eval `io_block` on
//!   writability.
//! - `create` / `bind` / `listen` / `setsockopt_reuse` and the last-error
//!   readers pass straight through.

use std::io;

use koja_runtime::{ConnectProgress, connect_finish, connect_start, set_last_error};
use koja_runtime_core::{Interest, IoWait, deadline_from_user_millis};

use crate::error::RuntimeError;
use crate::externs::marshal::{pass_through_externs, type_mismatch};
use crate::reactor;
use crate::value::Value;

unsafe extern "C" {
    fn koja_socket_accept(fd: i32, timeout_ms: i64) -> i32;
    fn koja_socket_create(sock_type: i64) -> i32;
    fn koja_socket_send_to(
        fd: i32,
        data: *const u8,
        data_length: i64,
        ip: *const u8,
        ip_length: i64,
        port: i64,
    ) -> i64;
    fn koja_socket_try_accept(fd: i32) -> i32;
}

pass_through_externs! {
    errno_code => fn koja_errno_code() -> Int32;
    last_error_code => fn koja_last_error_code() -> Int32;
    socket_bind => fn koja_socket_bind(
        fd: Int32,
        ip: CPtr,
        ip_length: Int64,
        port: Int64,
    ) -> Int64;
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

/// `koja_socket_accept(fd, timeout_ms)`: wait for the listener to be
/// readable (a pending connection), then delegate to the native blocking
/// accept, which now completes on its first syscall. Returns -1 on
/// error, interrupt, or timeout.
pub(super) async fn socket_accept(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Int(fd), Value::Int(timeout_ms)] = args else {
        return Err(type_mismatch(
            "koja_socket_accept",
            "(fd: Int32, timeout_ms: Int64)",
            args,
        ));
    };
    let deadline = deadline_from_user_millis(*timeout_ms);
    match reactor::io_block(*fd as i32, Interest::Readable, deadline).await {
        IoWait::Ready => {}
        IoWait::Interrupted => return Ok(Value::Int(-1)),
        IoWait::TimedOut => {
            reactor::note_timed_out();
            return Ok(Value::Int(-1));
        }
    }
    let client = unsafe { koja_socket_accept(*fd as i32, -1) };
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

/// `koja_socket_connect(fd, ip, ip_length, port, timeout_ms)`: start the
/// handshake, wait for writability on eval's reactor, then check the
/// outcome. Returns 0 on success, -1 on error or timeout.
pub(super) async fn socket_connect(args: &[Value]) -> Result<Value, RuntimeError> {
    let [
        Value::Int(fd),
        Value::CPtr(ip),
        Value::Int(ip_length),
        Value::Int(port),
        Value::Int(timeout_ms),
    ] = args
    else {
        return Err(type_mismatch(
            "koja_socket_connect",
            "(fd: Int32, ip: CPtr, ip_length: Int64, port: Int64, timeout_ms: Int64)",
            args,
        ));
    };
    let fd = *fd as i32;
    let deadline = deadline_from_user_millis(*timeout_ms);
    let mut progress = match connect_start(fd, *ip, *ip_length, *port) {
        Ok(progress) => progress,
        Err(e) => {
            set_last_error(e);
            return Ok(Value::Int(-1));
        }
    };
    while progress == ConnectProgress::InProgress {
        match reactor::io_block(fd, Interest::Writable, deadline).await {
            IoWait::Ready => {}
            IoWait::Interrupted => {
                set_last_error(io::Error::from(io::ErrorKind::Interrupted));
                return Ok(Value::Int(-1));
            }
            IoWait::TimedOut => {
                reactor::note_timed_out();
                return Ok(Value::Int(-1));
            }
        }
        progress = match connect_finish(fd) {
            Ok(progress) => progress,
            Err(e) => {
                set_last_error(e);
                return Ok(Value::Int(-1));
            }
        };
    }
    Ok(Value::Int(0))
}

/// `koja_socket_send_to(fd, data, data_length, ip, ip_length, port)`: wait for the socket to be
/// writable, then delegate to the native sender.
pub(super) async fn socket_send_to(args: &[Value]) -> Result<Value, RuntimeError> {
    let [
        Value::Int(fd),
        Value::CPtr(data),
        Value::Int(data_length),
        Value::CPtr(ip),
        Value::Int(ip_length),
        Value::Int(port),
    ] = args
    else {
        return Err(type_mismatch(
            "koja_socket_send_to",
            "(fd: Int32, data: CPtr, data_length: Int64, ip: CPtr, ip_length: Int64, port: Int64)",
            args,
        ));
    };
    // If interrupted by a signal, return the native -1 sentinel.
    if reactor::io_block(*fd as i32, Interest::Writable, None).await != IoWait::Ready {
        return Ok(Value::Int(-1));
    }
    let sent =
        unsafe { koja_socket_send_to(*fd as i32, *data, *data_length, *ip, *ip_length, *port) };
    Ok(Value::Int(sent))
}

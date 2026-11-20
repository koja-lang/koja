//! Externs declared in `lib/net/src/net.koja` and
//! `lib/net/src/error.koja`.
//!
//! Eval reuses the runtime's `koja_socket_*` symbols over the
//! non-blocking fds the runtime creates, waiting for readiness through
//! [`crate::reactor`] before each native call (see there for why).
//!
//! - `accept` / `send_to` [`io_block`](crate::reactor::io_block) for
//!   readiness, then call the native symbol. A zero `timeout_ms` makes
//!   `accept` a poll, since `io_block` checks readiness before the
//!   deadline.
//! - `connect` cannot pre-wait (the fd is not writable until the
//!   handshake is initiated), so it drives the runtime's split
//!   `connect_start` / `connect_finish` around an eval `io_block` on
//!   writability.
//! - `create` / `bind` / `listen` / `setsockopt_nodelay` /
//!   `setsockopt_reuse` and the errno reader pass straight through.
//!
//! A wait that ends without readiness returns the same failure value
//! the native symbol returns for that cause.

use koja_runtime::{ConnectProgress, connect_finish, connect_start, failure};
use koja_runtime_core::{Interest, deadline_from_user_millis};

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
}

pass_through_externs! {
    errno_failure => fn koja_errno_failure() -> Int32;
    socket_bind => fn koja_socket_bind(
        fd: Int32,
        ip: CPtr,
        ip_length: Int64,
        port: Int64,
    ) -> Int64;
    socket_listen => fn koja_socket_listen(fd: Int32, backlog: Int64) -> Int64;
    socket_setsockopt_nodelay => fn koja_socket_setsockopt_nodelay(
        fd: Int32,
        enabled: Int64,
    ) -> Int64;
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

/// Handles `koja_socket_accept(fd, timeout_ms)`. Waits for the listener
/// to be readable (a pending connection), then delegates to the native
/// blocking accept, which now completes on its first syscall. Returns
/// the client fd, or the failure value of an error, interrupt, or
/// timeout.
pub(super) async fn socket_accept(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Int(fd), Value::Int(timeout_ms)] = args else {
        return Err(type_mismatch(
            "koja_socket_accept",
            "(fd: Int32, timeout_ms: Int64)",
            args,
        ));
    };
    let deadline = deadline_from_user_millis(*timeout_ms);
    let wait = reactor::io_block(*fd as i32, Interest::Readable, deadline).await;
    if let Some(failed) = reactor::wait_failure(wait) {
        return Ok(Value::Int(failed));
    }
    let client = unsafe { koja_socket_accept(*fd as i32, -1) };
    Ok(Value::Int(i64::from(client)))
}

/// Handles `koja_socket_connect(fd, ip, ip_length, port, timeout_ms)`.
/// Starts the handshake, waits for writability on eval's reactor, then
/// checks the outcome. Returns 0, or the failure value of an error,
/// interrupt, or timeout.
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
        Err(e) => return Ok(Value::Int(failure(&e))),
    };
    while progress == ConnectProgress::InProgress {
        let wait = reactor::io_block(fd, Interest::Writable, deadline).await;
        if let Some(failed) = reactor::wait_failure(wait) {
            return Ok(Value::Int(failed));
        }
        progress = match connect_finish(fd) {
            Ok(progress) => progress,
            Err(e) => return Ok(Value::Int(failure(&e))),
        };
    }
    Ok(Value::Int(0))
}

/// Handles `koja_socket_send_to(fd, data, data_length, ip, ip_length,
/// port)`. Waits for the socket to be writable, then delegates to the
/// native sender. Returns the number of bytes sent, or the failure
/// value of an error or interrupt.
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
    let wait = reactor::io_block(*fd as i32, Interest::Writable, None).await;
    if let Some(failed) = reactor::wait_failure(wait) {
        return Ok(Value::Int(failed));
    }
    let sent =
        unsafe { koja_socket_send_to(*fd as i32, *data, *data_length, *ip, *ip_length, *port) };
    Ok(Value::Int(sent))
}

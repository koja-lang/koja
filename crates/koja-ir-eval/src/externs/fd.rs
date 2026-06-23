//! Externs declared in `lib/global/src/fd.koja`.
//!
//! Three families:
//!
//! - **Plain fd I/O** (`koja_fd_close` / `koja_fd_read` / `koja_fd_write`)
//!   — call into [`koja_runtime::fs`] over libc so eval and the LLVM
//!   backend observe the same kernel return values and koja-heap string
//!   layout. `read` / `write` first [`io_block`](crate::reactor::io_block)
//!   for readiness (cooperatively parking the process, or blocking the
//!   thread in function mode) so the native call's syscall succeeds on its
//!   first try — eval never reaches the native `io_block` it can't drive.
//! - **File-path operations** (`koja_file_*`) — wrap the runtime's
//!   path-based helpers; the runtime owns null-termination and CStr
//!   parsing on the C side. Regular files are always ready, so no `io_block`.
//! - **Actor-coupled I/O** (`koja_io_block`, `koja_rt_watch_fd`,
//!   `koja_rt_unwatch_fd`) — routed to eval's own cooperative
//!   [`crate::reactor`] (the native symbols are welded to the native
//!   scheduler), so `Fd.block` parks on readiness and `Fd.watch` delivers
//!   `IOReady` messages through the driver.
//!
//! `Value::Int` carries every sized-integer width inside eval; the
//! generated handlers narrow on the way out (`as i32`) at the C ABI
//! boundary.

use koja_runtime_core::Interest;

use crate::error::RuntimeError;
use crate::externs::marshal::{pass_through_externs, type_mismatch};
use crate::reactor;
use crate::scheduler;
use crate::value::Value;

unsafe extern "C" {
    fn koja_fd_read(fd: i32, count: i64) -> *mut u8;
    fn koja_fd_write(fd: i32, data: *mut u8, len: i64) -> i64;
}

pass_through_externs! {
    fd_close => fn koja_fd_close(fd: Int32) -> Int32;
    file_delete => fn koja_file_delete(path: CPtr) -> Int64;
    file_exists => fn koja_file_exists(path: CPtr) -> Int64;
    file_open => fn koja_file_open(path: CPtr, mode: Int64) -> Int32;
    file_read_all => fn koja_file_read_all(path: CPtr) -> CPtr;
    file_rename => fn koja_file_rename(src: CPtr, dst: CPtr) -> Int64;
    file_write_all => fn koja_file_write_all(path: CPtr, content: CPtr) -> Int64;
}

/// `koja_fd_read(fd, count)` — wait for `fd` to be readable, then delegate
/// to the native reader (which owns the koja-string marshaling). Returns
/// the length-prefixed string pointer, or null on error.
pub(super) async fn fd_read(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Int(fd), Value::Int(count)] = args else {
        return Err(type_mismatch(
            "koja_fd_read",
            "(fd: Int32, count: Int64)",
            args,
        ));
    };
    // Interrupted by a signal — return the native null sentinel.
    if reactor::io_block(*fd as i32, Interest::Readable).await {
        return Ok(Value::CPtr(std::ptr::null_mut()));
    }
    let ptr = unsafe { koja_fd_read(*fd as i32, *count) };
    Ok(Value::CPtr(ptr))
}

/// `koja_fd_write(fd, data, len)` — wait for `fd` to be writable, then
/// delegate to the native writer. Returns the bytes written, or -1.
pub(super) async fn fd_write(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Int(fd), Value::CPtr(data), Value::Int(len)] = args else {
        return Err(type_mismatch(
            "koja_fd_write",
            "(fd: Int32, data: CPtr, len: Int64)",
            args,
        ));
    };
    // Interrupted by a signal — return the native -1 sentinel.
    if reactor::io_block(*fd as i32, Interest::Writable).await {
        return Ok(Value::Int(-1));
    }
    let written = unsafe { koja_fd_write(*fd as i32, *data, *len) };
    Ok(Value::Int(written))
}

/// `koja_io_block(fd, readable)` (`Fd.block`) — suspend until `fd` is
/// ready for the requested direction via eval's reactor.
pub(super) async fn io_block(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Int(fd), Value::Int(readable)] = args else {
        return Err(type_mismatch(
            "koja_io_block",
            "(fd: Int32, readable: Int64)",
            args,
        ));
    };
    let interest = if *readable != 0 {
        Interest::Readable
    } else {
        Interest::Writable
    };
    let _ = reactor::io_block(*fd as i32, interest).await;
    Ok(Value::Unit)
}

/// `koja_rt_watch_fd(fd, interest)` (`Fd.watch`) — arm `fd` so the driver
/// delivers an `IOReady` message to the current process when it fires.
/// `interest`: 0 = readable, 1 = writable.
pub(super) fn rt_watch_fd(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Int(fd), Value::Int(interest)] = args else {
        return Err(type_mismatch(
            "koja_rt_watch_fd",
            "(fd: Int32, interest: Int64)",
            args,
        ));
    };
    let interest = if *interest == 1 {
        Interest::Writable
    } else {
        Interest::Readable
    };
    reactor::watch(*fd as i32, interest, scheduler::current_pid());
    Ok(Value::Unit)
}

/// `koja_rt_unwatch_fd(fd)` (`Fd.unwatch`) — stop monitoring `fd`.
pub(super) fn rt_unwatch_fd(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Int(fd)] = args else {
        return Err(type_mismatch("koja_rt_unwatch_fd", "(fd: Int32)", args));
    };
    reactor::unwatch(*fd as i32);
    Ok(Value::Unit)
}

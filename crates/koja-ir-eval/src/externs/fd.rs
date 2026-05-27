//! Externs declared in `lib/global/src/fd.koja`.
//!
//! Three families:
//!
//! - **Plain fd I/O** (`koja_fd_close` / `koja_fd_read` / `koja_fd_write`)
//!   — call straight into [`koja_runtime::fs`] over libc so eval and
//!   the LLVM backend observe the same kernel return values.
//! - **File-path operations** (`koja_file_*`) — wrap the runtime's
//!   path-based helpers; the runtime owns null-termination and CStr
//!   parsing on the C side.
//! - **Actor-coupled I/O** (`koja_io_block`, `koja_rt_watch_fd`,
//!   `koja_rt_unwatch_fd`) — register here so dispatch routes them,
//!   but they require an initialized scheduler / reactor in the
//!   runtime; calling them from a plain `koja eval` panics inside
//!   the runtime's `REACTOR.get().expect(...)`. That's the same
//!   behavior the LLVM backend exhibits when the runtime hasn't been
//!   booted, so the byte-equivalent contract holds.
//!
//! `Value::Int` carries every sized-integer width inside eval; we
//! narrow on the way out (`*fd as i32`) at the C ABI boundary.

use crate::error::RuntimeError;
use crate::value::Value;

unsafe extern "C" {
    fn koja_fd_close(fd: i32) -> i32;
    fn koja_fd_read(fd: i32, count: i64) -> *const u8;
    fn koja_fd_write(fd: i32, data_ptr: *const u8, data_len: i64) -> i64;
    fn koja_file_delete(path_ptr: *const u8) -> i64;
    fn koja_file_exists(path_ptr: *const u8) -> i64;
    fn koja_file_open(path_ptr: *const u8, mode: i64) -> i32;
    fn koja_file_read_all(path_ptr: *const u8) -> *const u8;
    fn koja_file_rename(src_ptr: *const u8, dst_ptr: *const u8) -> i64;
    fn koja_file_write_all(path_ptr: *const u8, content_ptr: *const u8) -> i64;
    fn koja_io_block(fd: i32, readable: i64);
    fn koja_rt_unwatch_fd(fd: i32);
    fn koja_rt_watch_fd(fd: i32, interest: i64);
}

pub(super) fn fd_close(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Int(fd)] = args else {
        return Err(type_mismatch("koja_fd_close", "(fd: Int32)", args));
    };
    let ret = unsafe { koja_fd_close(*fd as i32) };
    Ok(Value::Int(ret as i64))
}

pub(super) fn fd_read(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Int(fd), Value::Int(count)] = args else {
        return Err(type_mismatch(
            "koja_fd_read",
            "(fd: Int32, count: Int64)",
            args,
        ));
    };
    let ptr = unsafe { koja_fd_read(*fd as i32, *count) };
    Ok(Value::CPtr(ptr as *mut u8))
}

pub(super) fn fd_write(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Int(fd), Value::CPtr(data), Value::Int(len)] = args else {
        return Err(type_mismatch(
            "koja_fd_write",
            "(fd: Int32, data: CPtr<UInt8>, len: Int64)",
            args,
        ));
    };
    let ret = unsafe { koja_fd_write(*fd as i32, *data as *const u8, *len) };
    Ok(Value::Int(ret))
}

pub(super) fn file_delete(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::CPtr(path)] = args else {
        return Err(type_mismatch(
            "koja_file_delete",
            "(path: CPtr<UInt8>)",
            args,
        ));
    };
    let ret = unsafe { koja_file_delete(*path as *const u8) };
    Ok(Value::Int(ret))
}

pub(super) fn file_exists(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::CPtr(path)] = args else {
        return Err(type_mismatch(
            "koja_file_exists",
            "(path: CPtr<UInt8>)",
            args,
        ));
    };
    let ret = unsafe { koja_file_exists(*path as *const u8) };
    Ok(Value::Int(ret))
}

pub(super) fn file_open(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::CPtr(path), Value::Int(mode)] = args else {
        return Err(type_mismatch(
            "koja_file_open",
            "(path: CPtr<UInt8>, mode: Int64)",
            args,
        ));
    };
    let ret = unsafe { koja_file_open(*path as *const u8, *mode) };
    Ok(Value::Int(ret as i64))
}

pub(super) fn file_read_all(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::CPtr(path)] = args else {
        return Err(type_mismatch(
            "koja_file_read_all",
            "(path: CPtr<UInt8>)",
            args,
        ));
    };
    let ptr = unsafe { koja_file_read_all(*path as *const u8) };
    Ok(Value::CPtr(ptr as *mut u8))
}

pub(super) fn file_rename(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::CPtr(src), Value::CPtr(dst)] = args else {
        return Err(type_mismatch(
            "koja_file_rename",
            "(src: CPtr<UInt8>, dst: CPtr<UInt8>)",
            args,
        ));
    };
    let ret = unsafe { koja_file_rename(*src as *const u8, *dst as *const u8) };
    Ok(Value::Int(ret))
}

pub(super) fn file_write_all(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::CPtr(path), Value::CPtr(content)] = args else {
        return Err(type_mismatch(
            "koja_file_write_all",
            "(path: CPtr<UInt8>, content: CPtr<UInt8>)",
            args,
        ));
    };
    let ret = unsafe { koja_file_write_all(*path as *const u8, *content as *const u8) };
    Ok(Value::Int(ret))
}

pub(super) fn io_block(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Int(fd), Value::Int(readable)] = args else {
        return Err(type_mismatch(
            "koja_io_block",
            "(fd: Int32, readable: Int64)",
            args,
        ));
    };
    unsafe { koja_io_block(*fd as i32, *readable) };
    Ok(Value::Unit)
}

pub(super) fn rt_unwatch_fd(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Int(fd)] = args else {
        return Err(type_mismatch("koja_rt_unwatch_fd", "(fd: Int32)", args));
    };
    unsafe { koja_rt_unwatch_fd(*fd as i32) };
    Ok(Value::Unit)
}

pub(super) fn rt_watch_fd(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Int(fd), Value::Int(interest)] = args else {
        return Err(type_mismatch(
            "koja_rt_watch_fd",
            "(fd: Int32, interest: Int64)",
            args,
        ));
    };
    unsafe { koja_rt_watch_fd(*fd as i32, *interest) };
    Ok(Value::Unit)
}

fn type_mismatch(name: &str, signature: &str, args: &[Value]) -> RuntimeError {
    RuntimeError::TypeMismatch {
        detail: format!(
            "{name} expects {signature}; got {} arg(s): {args:?}",
            args.len(),
        ),
    }
}

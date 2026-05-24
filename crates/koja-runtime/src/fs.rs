//! File descriptor and file system runtime functions.

use std::ffi::{CStr, c_char};
use std::fs::{self, OpenOptions};
use std::io;
use std::os::fd::IntoRawFd;
use std::path::Path;
use std::ptr;

use crate::ffi::{EAGAIN, get_errno, libc_close, libc_read, libc_write};
use crate::reactor::{Interest, io_block};
use crate::util::{alloc_koja_string, set_last_error};

/// Closes a raw file descriptor. Returns 0 on success, -1 on error.
#[unsafe(no_mangle)]
pub extern "C" fn koja_fd_close(fd: i32) -> i32 {
    let ret = unsafe { libc_close(fd) };
    if ret < 0 {
        set_last_error(io::Error::last_os_error());
        return -1;
    }
    0
}

/// Reads up to `count` bytes from a raw file descriptor. If the fd is
/// non-blocking (sockets), suspends the process until data is available.
/// Returns a length-prefixed string pointer, or null on error.
#[unsafe(no_mangle)]
pub extern "C" fn koja_fd_read(fd: i32, count: i64) -> *const u8 {
    let mut buf = vec![0u8; count as usize];
    loop {
        let n = unsafe { libc_read(fd, buf.as_mut_ptr(), buf.len()) };
        if n >= 0 {
            buf.truncate(n as usize);
            return unsafe { alloc_koja_string(&buf) };
        }
        if get_errno() == EAGAIN {
            io_block(fd, Interest::Readable);
            continue;
        }
        set_last_error(io::Error::last_os_error());
        return ptr::null();
    }
}

/// Writes `data_len` bytes from `data_ptr` to a raw file descriptor.
/// If the fd is non-blocking (sockets), suspends the process until the
/// write buffer has space. Returns bytes written, or -1 on error.
///
/// # Safety
/// `data_ptr` must point to at least `data_len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_fd_write(fd: i32, data_ptr: *const u8, data_len: i64) -> i64 {
    let slice = unsafe { std::slice::from_raw_parts(data_ptr, data_len as usize) };
    loop {
        let n = unsafe { libc_write(fd, slice.as_ptr(), slice.len()) };
        if n >= 0 {
            return n as i64;
        }
        if get_errno() == EAGAIN {
            io_block(fd, Interest::Writable);
            continue;
        }
        set_last_error(io::Error::last_os_error());
        return -1;
    }
}

/// Deletes the file at `path`. Returns 0 on success, -1 on error.
///
/// # Safety
/// `path_ptr` must point to a valid NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_file_delete(path_ptr: *const u8) -> i64 {
    let path = unsafe { CStr::from_ptr(path_ptr as *const c_char) };
    let path = match path.to_str() {
        Ok(s) => s,
        Err(e) => {
            set_last_error(e);
            return -1;
        }
    };
    match fs::remove_file(path) {
        Ok(()) => 0,
        Err(e) => {
            set_last_error(e);
            -1
        }
    }
}

/// Returns 1 if the file at `path` exists, 0 otherwise.
///
/// # Safety
/// `path_ptr` must point to a valid NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_file_exists(path_ptr: *const u8) -> i64 {
    let path = unsafe { CStr::from_ptr(path_ptr as *const c_char) };
    let path = match path.to_str() {
        Ok(s) => s,
        Err(e) => {
            set_last_error(e);
            return 0;
        }
    };
    if Path::new(path).exists() { 1 } else { 0 }
}

/// Opens a file. `mode`: 0 = read, 1 = write (create/truncate), 2 = append.
/// Returns fd on success, -1 on error.
///
/// # Safety
/// `path_ptr` must point to a valid NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_file_open(path_ptr: *const u8, mode: i64) -> i32 {
    let path = unsafe { CStr::from_ptr(path_ptr as *const c_char) };
    let path = match path.to_str() {
        Ok(s) => s,
        Err(e) => {
            set_last_error(e);
            return -1;
        }
    };

    let file = match mode {
        0 => OpenOptions::new().read(true).open(path),
        1 => OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path),
        2 => OpenOptions::new().append(true).create(true).open(path),
        _ => {
            set_last_error("invalid file open mode");
            return -1;
        }
    };

    match file {
        Ok(f) => f.into_raw_fd(),
        Err(e) => {
            set_last_error(e);
            -1
        }
    }
}

/// Reads the entire contents of a file as a length-prefixed string.
///
/// # Safety
/// `path_ptr` must point to a valid NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_file_read_all(path_ptr: *const u8) -> *const u8 {
    let path = unsafe { CStr::from_ptr(path_ptr as *const c_char) };
    let path = match path.to_str() {
        Ok(s) => s,
        Err(e) => {
            set_last_error(e);
            return ptr::null();
        }
    };

    match fs::read(path) {
        Ok(bytes) => unsafe { alloc_koja_string(&bytes) },
        Err(e) => {
            set_last_error(e);
            ptr::null()
        }
    }
}

/// Renames `src` to `dst`. Returns 0 on success, -1 on error.
///
/// # Safety
/// Both pointers must point to valid NUL-terminated UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_file_rename(src_ptr: *const u8, dst_ptr: *const u8) -> i64 {
    let src = unsafe { CStr::from_ptr(src_ptr as *const c_char) };
    let dst = unsafe { CStr::from_ptr(dst_ptr as *const c_char) };
    let src = match src.to_str() {
        Ok(s) => s,
        Err(e) => {
            set_last_error(e);
            return -1;
        }
    };
    let dst = match dst.to_str() {
        Ok(s) => s,
        Err(e) => {
            set_last_error(e);
            return -1;
        }
    };
    match fs::rename(src, dst) {
        Ok(()) => 0,
        Err(e) => {
            set_last_error(e);
            -1
        }
    }
}

/// Writes `content` to the file at `path`, creating or truncating it.
///
/// # Safety
/// Both pointers must point to valid NUL-terminated UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_file_write_all(path_ptr: *const u8, content_ptr: *const u8) -> i64 {
    let path = unsafe { CStr::from_ptr(path_ptr as *const c_char) };
    let path = match path.to_str() {
        Ok(s) => s,
        Err(e) => {
            set_last_error(e);
            return -1;
        }
    };
    let content = unsafe { CStr::from_ptr(content_ptr as *const c_char) };
    let data = content.to_bytes();
    match fs::write(path, data) {
        Ok(()) => 0,
        Err(e) => {
            set_last_error(e);
            -1
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn koja_io_block(fd: i32, readable: i64) {
    let interest = if readable != 0 {
        Interest::Readable
    } else {
        Interest::Writable
    };
    io_block(fd, interest);
}

//! File descriptor and file system runtime functions.

use std::ffi::{CStr, c_char};
use std::fs::{self, OpenOptions};
use std::os::fd::IntoRawFd;
use std::path::Path;
use std::slice;

use koja_runtime_core::{IoWait, deadline_from_user_millis};

use crate::ffi::{libc_read, libc_write};
use crate::reactor::{Interest, block_until_ready, io_block, release_fd_and_close};
use crate::signals;
use crate::util::{FAILURE_INVALID_PATH, FAILURE_NO_ERRNO, alloc_binary, failure};

/// Decodes a NUL-terminated C path pointer into `&str`. A path that is
/// not valid UTF-8 is the `InvalidPath` cause, and the `Err` holds its
/// [`failure`] value for the caller to return.
///
/// # Safety
/// `ptr` must point to a valid NUL-terminated string.
unsafe fn cstr_path<'a>(ptr: *const u8) -> Result<&'a str, i64> {
    unsafe { CStr::from_ptr(ptr as *const c_char) }
        .to_str()
        .map_err(|_| FAILURE_INVALID_PATH)
}

/// Closes a raw file descriptor. Returns 0, or the [`failure`] value of
/// the error. The reactor owns the close so its registration teardown
/// and the `close(2)` syscall stay atomic with respect to concurrent
/// parkers.
#[unsafe(no_mangle)]
pub extern "C" fn koja_fd_close(fd: i32) -> i32 {
    match release_fd_and_close(fd) {
        Ok(()) => 0,
        Err(e) => failure(&e) as i32,
    }
}

/// Reads up to `count` bytes from a raw file descriptor into `buffer`.
/// If the fd is non-blocking (sockets), suspends the process until data
/// is available or `timeout_ms` passes (negative for no limit), which
/// fails with `TimedOut`. Returns the number of bytes read, or the
/// [`failure`] value of the error.
///
/// # Safety
/// `buffer` must point to at least `count` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_fd_read(
    fd: i32,
    buffer: *mut u8,
    count: i64,
    timeout_ms: i64,
) -> i64 {
    let deadline = deadline_from_user_millis(timeout_ms);
    match block_until_ready(fd, Interest::Readable, deadline, || unsafe {
        libc_read(fd, buffer, count as usize)
    }) {
        Ok(n) => n as i64,
        Err(e) => failure(&e),
    }
}

/// Writes `data_len` bytes from `data_ptr` to a raw file descriptor.
/// If the fd is non-blocking (sockets), suspends the process until the
/// write buffer has space or `timeout_ms` passes (negative for no
/// limit), which fails with `TimedOut`. Returns the number of bytes
/// written, or the [`failure`] value of the error.
///
/// # Safety
/// `data_ptr` must point to at least `data_len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_fd_write(
    fd: i32,
    data_ptr: *const u8,
    data_len: i64,
    timeout_ms: i64,
) -> i64 {
    let slice = unsafe { std::slice::from_raw_parts(data_ptr, data_len as usize) };
    let deadline = deadline_from_user_millis(timeout_ms);
    match block_until_ready(fd, Interest::Writable, deadline, || unsafe {
        libc_write(fd, slice.as_ptr(), slice.len())
    }) {
        Ok(n) => n as i64,
        Err(e) => {
            if (fd == 1 || fd == 2) && e.raw_os_error() == Some(libc::EPIPE) {
                signals::die_from_sigpipe();
            }
            failure(&e)
        }
    }
}

/// Deletes the file at `path`. Returns 0, or the [`failure`] value of
/// the error.
///
/// # Safety
/// `path_ptr` must point to a valid NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_file_delete(path_ptr: *const u8) -> i64 {
    let path = match unsafe { cstr_path(path_ptr) } {
        Ok(path) => path,
        Err(invalid) => return invalid,
    };
    match fs::remove_file(path) {
        Ok(()) => 0,
        Err(e) => failure(&e),
    }
}

/// Returns 1 if the file at `path` exists, 0 otherwise.
///
/// # Safety
/// `path_ptr` must point to a valid NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_file_exists(path_ptr: *const u8) -> i64 {
    let Ok(path) = (unsafe { cstr_path(path_ptr) }) else {
        return 0;
    };
    if Path::new(path).exists() { 1 } else { 0 }
}

/// Returns 1 if `path` is a directory, 0 otherwise.
///
/// # Safety
/// `path_ptr` must point to a valid NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_file_is_dir(path_ptr: *const u8) -> i64 {
    let Ok(path) = (unsafe { cstr_path(path_ptr) }) else {
        return 0;
    };
    if Path::new(path).is_dir() { 1 } else { 0 }
}

/// Creates a single directory. Errors if the parent is missing or the
/// path already exists. Returns 0, or the [`failure`] value of the
/// error.
///
/// # Safety
/// `path_ptr` must point to a valid NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_file_mkdir(path_ptr: *const u8) -> i64 {
    let path = match unsafe { cstr_path(path_ptr) } {
        Ok(path) => path,
        Err(invalid) => return invalid,
    };
    match fs::create_dir(path) {
        Ok(()) => 0,
        Err(e) => failure(&e),
    }
}

/// Creates a directory and any missing parents (`mkdir -p`). An already
/// existing directory succeeds. Returns 0, or the [`failure`] value of
/// the error.
///
/// # Safety
/// `path_ptr` must point to a valid NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_file_mkdir_p(path_ptr: *const u8) -> i64 {
    let path = match unsafe { cstr_path(path_ptr) } {
        Ok(path) => path,
        Err(invalid) => return invalid,
    };
    match fs::create_dir_all(path) {
        Ok(()) => 0,
        Err(e) => failure(&e),
    }
}

/// Removes an empty directory. Returns 0, or the [`failure`] value of
/// the error.
///
/// # Safety
/// `path_ptr` must point to a valid NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_file_rmdir(path_ptr: *const u8) -> i64 {
    let path = match unsafe { cstr_path(path_ptr) } {
        Ok(path) => path,
        Err(invalid) => return invalid,
    };
    match fs::remove_dir(path) {
        Ok(()) => 0,
        Err(e) => failure(&e),
    }
}

/// Opens a file. A `mode` of 0 is read, 1 is write (create or
/// truncate), and 2 is append. Returns the fd, or the [`failure`] value
/// of the error.
///
/// # Safety
/// `path_ptr` must point to a valid NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_file_open(path_ptr: *const u8, mode: i64) -> i32 {
    let path = match unsafe { cstr_path(path_ptr) } {
        Ok(path) => path,
        Err(invalid) => return invalid as i32,
    };

    let file = match mode {
        0 => OpenOptions::new().read(true).open(path),
        1 => OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path),
        2 => OpenOptions::new().append(true).create(true).open(path),
        _ => return FAILURE_NO_ERRNO as i32,
    };

    match file {
        Ok(f) => f.into_raw_fd(),
        Err(e) => failure(&e) as i32,
    }
}

/// Reads the entire contents of a file as a length-prefixed Binary.
/// Writes the Binary pointer through `out` and returns 0. On an error
/// it returns the [`failure`] value and does not write `out`.
///
/// # Safety
/// `path_ptr` must point to a valid NUL-terminated UTF-8 string, and
/// `out` must point to a writable pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_file_read_all(path_ptr: *const u8, out: *mut *const u8) -> i64 {
    let path = match unsafe { cstr_path(path_ptr) } {
        Ok(path) => path,
        Err(invalid) => return invalid,
    };

    match fs::read(path) {
        Ok(bytes) => {
            unsafe { *out = alloc_binary(&bytes) };
            0
        }
        Err(e) => failure(&e),
    }
}

/// Renames `src` to `dst`. Returns 0, or the [`failure`] value of the
/// error.
///
/// # Safety
/// Both pointers must point to valid NUL-terminated UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_file_rename(src_ptr: *const u8, dst_ptr: *const u8) -> i64 {
    let src = match unsafe { cstr_path(src_ptr) } {
        Ok(path) => path,
        Err(invalid) => return invalid,
    };
    let dst = match unsafe { cstr_path(dst_ptr) } {
        Ok(path) => path,
        Err(invalid) => return invalid,
    };
    match fs::rename(src, dst) {
        Ok(()) => 0,
        Err(e) => failure(&e),
    }
}

/// Writes `content` to the file at `path`, creating or truncating it.
/// Returns 0, or the [`failure`] value of the error.
///
/// # Safety
/// `path_ptr` must be a NUL-terminated UTF-8 string and `content_ptr`
/// must point to `content_len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_file_write_all(
    path_ptr: *const u8,
    content_ptr: *const u8,
    content_len: i64,
) -> i64 {
    let path = match unsafe { cstr_path(path_ptr) } {
        Ok(path) => path,
        Err(invalid) => return invalid,
    };
    let data = unsafe { slice::from_raw_parts(content_ptr, content_len as usize) };
    match fs::write(path, data) {
        Ok(()) => 0,
        Err(e) => failure(&e),
    }
}

/// `IO.Descriptor.block`: suspends the process until `fd` is ready for
/// the requested direction or `timeout_ms` passes (negative for no
/// limit). Returns 1 when the deadline passed, 0 otherwise.
///
/// `interest`: 0 = readable, 1 = writable, the same encoding as
/// `koja_rt_watch_fd`.
#[unsafe(no_mangle)]
pub extern "C" fn koja_io_block(fd: i32, interest: i64, timeout_ms: i64) -> i64 {
    let interest = if interest == 1 {
        Interest::Writable
    } else {
        Interest::Readable
    };
    let deadline = deadline_from_user_millis(timeout_ms);
    let wait = io_block(fd, interest, deadline);
    i64::from(wait == IoWait::TimedOut)
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;
    use crate::util::error_kind_code;

    #[test]
    fn write_to_closed_pipe_fails_with_broken_pipe() {
        let _guard = signals::SIGPIPE_TEST_LOCK
            .lock()
            .expect("sigpipe test lock");
        signals::install();

        let mut fds = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe(2)");
        let [read_end, write_end] = fds;
        unsafe { libc::close(read_end) };

        let data = b"x";
        let written = unsafe { koja_fd_write(write_end, data.as_ptr(), data.len() as i64, -1) };

        assert_eq!(
            written,
            -i64::from(error_kind_code(io::ErrorKind::BrokenPipe))
        );
        unsafe { libc::close(write_end) };
    }
}

//! System information and time runtime functions.

use std::env;
use std::ffi::{CStr, CString, c_char};
use std::ptr;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::ffi::get_errno;
#[cfg(target_os = "macos")]
use crate::ffi::libc_getentropy;
use crate::ffi::libc_gethostname;
#[cfg(target_os = "linux")]
use crate::ffi::libc_getrandom;
use crate::util::{alloc_binary, set_last_error};

/// Allocates a NUL-terminated C string copy of `s` and hands ownership
/// to the caller as a raw payload pointer. Panics on an interior NUL,
/// which OS paths and environment values never contain.
fn into_raw_cstring(s: impl Into<Vec<u8>>) -> *const u8 {
    CString::new(s).unwrap().into_raw() as *const u8
}

/// Returns the current working directory, or null on error.
#[unsafe(no_mangle)]
pub extern "C" fn koja_cwd() -> *const u8 {
    match env::current_dir() {
        Ok(path) => into_raw_cstring(path.to_string_lossy().into_owned()),
        Err(e) => {
            set_last_error(e);
            ptr::null()
        }
    }
}

/// Returns the value of environment variable `key`, or null if not set.
///
/// # Safety
/// `key_ptr` must point to a valid NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_get_env(key_ptr: *const u8) -> *const u8 {
    let key = unsafe { CStr::from_ptr(key_ptr as *const c_char) };
    let key = match key.to_str() {
        Ok(s) => s,
        Err(_) => return ptr::null(),
    };
    match env::var(key) {
        Ok(val) => into_raw_cstring(val),
        Err(env::VarError::NotPresent) => ptr::null(),
        Err(env::VarError::NotUnicode(_)) => {
            panic!("environment variable `{key}` is not valid UTF-8")
        }
    }
}

/// Returns the system hostname.
#[unsafe(no_mangle)]
pub extern "C" fn koja_hostname() -> *const u8 {
    let mut buf = [0u8; 256];
    let ret = unsafe { libc_gethostname(buf.as_mut_ptr() as *mut c_char, buf.len()) };
    if ret != 0 {
        return into_raw_cstring("unknown");
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(0);
    into_raw_cstring(String::from_utf8_lossy(&buf[..len]).into_owned())
}

/// Sets the environment variable `key` to `value`.
///
/// # Safety
/// Both pointers must point to valid NUL-terminated UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_set_env(key_ptr: *const u8, val_ptr: *const u8) {
    let key = unsafe { CStr::from_ptr(key_ptr as *const c_char) };
    let val = unsafe { CStr::from_ptr(val_ptr as *const c_char) };
    if let (Ok(k), Ok(v)) = (key.to_str(), val.to_str()) {
        unsafe { env::set_var(k, v) };
    }
}

/// Returns the current wall-clock time as milliseconds since the Unix epoch.
#[unsafe(no_mangle)]
pub extern "C" fn koja_time_now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Terminates the process immediately with the given exit code.
#[unsafe(no_mangle)]
pub extern "C" fn koja_kernel_exit(code: i64) {
    std::process::exit(code as i32);
}

/// Fills `buf` with `len` bytes of OS entropy. Retries transient `EINTR`
/// interruptions. A genuine failure panics (the global runtime hook turns
/// that into a clean diagnostic abort rather than a leaked error).
fn fill_random(buf: *mut u8, len: usize) {
    let mut offset = 0;
    while offset < len {
        let remaining = len - offset;
        let dest = unsafe { buf.add(offset) };

        #[cfg(target_os = "macos")]
        {
            let chunk = remaining.min(256);
            if unsafe { libc_getentropy(dest, chunk) } != 0 {
                let errno = get_errno();
                assert!(errno == libc::EINTR, "getentropy failed (errno {errno})");
                continue;
            }
            offset += chunk;
        }

        #[cfg(target_os = "linux")]
        {
            let n = unsafe { libc_getrandom(dest, remaining, 0) };
            if n < 0 {
                let errno = get_errno();
                assert!(errno == libc::EINTR, "getrandom failed (errno {errno})");
                continue;
            }
            offset += n as usize;
        }
    }
}

/// Returns a Binary containing `count` cryptographically random bytes.
#[unsafe(no_mangle)]
pub extern "C" fn koja_random_bytes(count: i64) -> *mut u8 {
    let len = count.max(0) as usize;
    let mut buf = vec![0u8; len];
    if len > 0 {
        fill_random(buf.as_mut_ptr(), len);
    }
    alloc_binary(&buf)
}

/// Returns a random integer in the inclusive range [min, max].
/// Uses rejection sampling to avoid modulo bias.
#[unsafe(no_mangle)]
pub extern "C" fn koja_random_int(min: i64, max: i64) -> i64 {
    if min >= max {
        return min;
    }
    let range = (max - min) as u64 + 1;
    let reject_above = u64::MAX - (u64::MAX % range);
    loop {
        let mut raw = [0u8; 8];
        fill_random(raw.as_mut_ptr(), 8);
        let val = u64::from_ne_bytes(raw);
        if val < reject_above {
            return min + (val % range) as i64;
        }
    }
}

//! System information and time runtime functions.

use std::env;
use std::ffi::{CStr, CString};
use std::ptr;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::ffi::libc_gethostname;
use crate::util::set_last_error;

/// Returns the current working directory, or null on error.
#[unsafe(no_mangle)]
pub extern "C" fn expo_cwd() -> *const u8 {
    match env::current_dir() {
        Ok(path) => {
            let s = path.to_string_lossy().into_owned();
            let c = CString::new(s).unwrap();
            c.into_raw() as *const u8
        }
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
pub unsafe extern "C" fn expo_get_env(key_ptr: *const u8) -> *const u8 {
    let key = unsafe { CStr::from_ptr(key_ptr as *const i8) };
    let key = match key.to_str() {
        Ok(s) => s,
        Err(_) => return ptr::null(),
    };
    match env::var(key) {
        Ok(val) => {
            let c = CString::new(val).unwrap();
            c.into_raw() as *const u8
        }
        Err(_) => ptr::null(),
    }
}

/// Returns the system hostname.
#[unsafe(no_mangle)]
pub extern "C" fn expo_hostname() -> *const u8 {
    let mut buf = [0u8; 256];
    let ret = unsafe { libc_gethostname(buf.as_mut_ptr() as *mut i8, buf.len()) };
    if ret != 0 {
        let c = CString::new("unknown").unwrap();
        return c.into_raw() as *const u8;
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(0);
    let s = String::from_utf8_lossy(&buf[..len]).into_owned();
    let c = CString::new(s).unwrap();
    c.into_raw() as *const u8
}

/// Sets the environment variable `key` to `value`.
///
/// # Safety
/// Both pointers must point to valid NUL-terminated UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn expo_set_env(key_ptr: *const u8, val_ptr: *const u8) {
    let key = unsafe { CStr::from_ptr(key_ptr as *const i8) };
    let val = unsafe { CStr::from_ptr(val_ptr as *const i8) };
    if let (Ok(k), Ok(v)) = (key.to_str(), val.to_str()) {
        unsafe { env::set_var(k, v) };
    }
}

/// Returns the current wall-clock time as milliseconds since the Unix epoch.
#[unsafe(no_mangle)]
pub extern "C" fn expo_time_now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

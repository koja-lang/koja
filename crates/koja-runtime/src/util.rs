//! Shared helpers used across multiple runtime modules.

use std::alloc;
use std::cell::RefCell;
use std::fmt;
use std::ptr;

use crate::ffi::malloc;

/// Size in bytes of the `i64` length header prepended to String/Binary payloads.
pub const STRING_HEADER_SIZE: usize = 8;
/// Number of bits in a byte, used for bit-length / byte-length conversions.
pub const BITS_PER_BYTE: usize = 8;

thread_local! {
    static LAST_IO_ERROR: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Allocates a Binary value with the given bytes (8-byte length header + data).
/// Returns a pointer to the payload (past the header).
pub fn alloc_binary(data: &[u8]) -> *mut u8 {
    let total = STRING_HEADER_SIZE + data.len();
    let base = unsafe { malloc(total) };
    let bit_len = (data.len() as i64) * BITS_PER_BYTE as i64;
    unsafe {
        *(base as *mut i64) = bit_len;
        let payload = base.add(STRING_HEADER_SIZE);
        ptr::copy_nonoverlapping(data.as_ptr(), payload, data.len());
        payload
    }
}

/// Allocates a length-prefixed Koja string from a byte slice.
/// Layout: `[i64 bit_length][payload...\0]`, returns pointer to payload.
///
/// # Safety
/// Caller must ensure the returned pointer is eventually freed.
pub unsafe fn alloc_koja_string(bytes: &[u8]) -> *const u8 {
    let byte_len = bytes.len();
    unsafe {
        let layout = alloc::Layout::from_size_align(STRING_HEADER_SIZE + byte_len + 1, 8).unwrap();
        let base = alloc::alloc(layout);
        let bit_len = (byte_len as i64) * BITS_PER_BYTE as i64;
        ptr::copy_nonoverlapping(
            &bit_len as *const i64 as *const u8,
            base,
            STRING_HEADER_SIZE,
        );
        let payload = base.add(STRING_HEADER_SIZE);
        ptr::copy_nonoverlapping(bytes.as_ptr(), payload, byte_len);
        *payload.add(byte_len) = 0;
        payload
    }
}

/// Returns the error message for the most recent failed I/O call.
#[unsafe(no_mangle)]
pub extern "C" fn koja_last_error() -> *const u8 {
    LAST_IO_ERROR.with(|cell| {
        let msg = cell.borrow();
        match msg.as_deref() {
            Some(s) => unsafe { alloc_koja_string(s.as_bytes()) },
            None => unsafe { alloc_koja_string(b"unknown error") },
        }
    })
}

/// Stores an error message in the thread-local `LAST_IO_ERROR` slot.
pub fn set_last_error(e: impl fmt::Display) {
    LAST_IO_ERROR.with(|cell| {
        *cell.borrow_mut() = Some(e.to_string());
    });
}

/// Matches the LLVM layout `{ ptr, i64, i64 }` used by `List<T>`.
#[repr(C)]
pub struct KojaList {
    pub ptr: *const u8,
    pub length: i64,
    pub capacity: i64,
}

/// Builds a `List<String>` from C `argc`/`argv` (skipping `argv[0]`, the
/// program name), converting each argument into a length-prefixed Koja
/// string and writing the result into `out`. Uses an output pointer to
/// avoid struct-return ABI mismatches on AArch64.
///
/// # Safety
/// `argv` must contain at least `argc` valid, NUL-terminated C strings.
/// `out` must point to writable memory large enough for an `KojaList`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_rt_build_argv(argc: i32, argv: *const *const u8, out: *mut KojaList) {
    let skip = 1; // skip argv[0] (program name)
    let total = argc.max(0) as usize;
    let count = total.saturating_sub(skip);
    let buf = unsafe { malloc(count * std::mem::size_of::<*const u8>()) as *mut *const u8 };
    for i in 0..count {
        let c_str =
            unsafe { std::ffi::CStr::from_ptr(*argv.add(i + skip) as *const std::ffi::c_char) };
        let koja_str = unsafe { alloc_koja_string(c_str.to_bytes()) };
        unsafe { *buf.add(i) = koja_str };
    }
    unsafe {
        *out = KojaList {
            ptr: buf as *const u8,
            length: count as i64,
            capacity: count as i64,
        };
    }
}

//! Shared helpers used across multiple runtime modules.

use crate::ffi::malloc;

thread_local! {
    static LAST_IO_ERROR: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Allocates a Binary value with the given bytes (8-byte length header + data).
/// Returns a pointer to the payload (past the header).
pub fn alloc_binary(data: &[u8]) -> *mut u8 {
    let total = 8 + data.len();
    let base = unsafe { malloc(total) };
    let bit_len = (data.len() as i64) * 8;
    unsafe {
        *(base as *mut i64) = bit_len;
        let payload = base.add(8);
        std::ptr::copy_nonoverlapping(data.as_ptr(), payload, data.len());
        payload
    }
}

/// Allocates a length-prefixed Expo string from a byte slice.
/// Layout: `[i64 bit_length][payload...\0]`, returns pointer to payload.
///
/// # Safety
/// Caller must ensure the returned pointer is eventually freed.
pub unsafe fn alloc_expo_string(bytes: &[u8]) -> *const u8 {
    let byte_len = bytes.len();
    unsafe {
        let layout = std::alloc::Layout::from_size_align(8 + byte_len + 1, 8).unwrap();
        let base = std::alloc::alloc(layout);
        let bit_len = (byte_len as i64) * 8;
        std::ptr::copy_nonoverlapping(&bit_len as *const i64 as *const u8, base, 8);
        let payload = base.add(8);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), payload, byte_len);
        *payload.add(byte_len) = 0;
        payload
    }
}

/// Returns the error message for the most recent failed I/O call.
#[unsafe(no_mangle)]
pub extern "C" fn expo_last_error() -> *const u8 {
    LAST_IO_ERROR.with(|cell| {
        let msg = cell.borrow();
        match msg.as_deref() {
            Some(s) => unsafe { alloc_expo_string(s.as_bytes()) },
            None => unsafe { alloc_expo_string(b"unknown error") },
        }
    })
}

/// Extracts the byte slice from a length-prefixed Expo string pointer.
///
/// # Safety
/// `ptr` must point to the payload of a valid length-prefixed Expo string
/// with an 8-byte bit-length header at offset -8.
pub unsafe fn expo_string_to_slice<'a>(ptr: *const u8) -> &'a [u8] {
    unsafe {
        let hdr = ptr.sub(8) as *const i64;
        let bit_len = *hdr;
        let byte_len = (bit_len / 8) as usize;
        std::slice::from_raw_parts(ptr, byte_len)
    }
}

/// Stores an error message in the thread-local `LAST_IO_ERROR` slot.
pub fn set_last_error(e: impl std::fmt::Display) {
    LAST_IO_ERROR.with(|cell| {
        *cell.borrow_mut() = Some(e.to_string());
    });
}

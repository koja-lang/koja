//! Shared helpers used across multiple runtime modules.

use std::alloc;
use std::cell::RefCell;
use std::fmt;
use std::ptr;
use std::slice;

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

/// Allocates a length-prefixed Expo string from a byte slice.
/// Layout: `[i64 bit_length][payload...\0]`, returns pointer to payload.
///
/// # Safety
/// Caller must ensure the returned pointer is eventually freed.
pub unsafe fn alloc_expo_string(bytes: &[u8]) -> *const u8 {
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
        let hdr = ptr.sub(STRING_HEADER_SIZE) as *const i64;
        let bit_len = *hdr;
        let byte_len = (bit_len / BITS_PER_BYTE as i64) as usize;
        slice::from_raw_parts(ptr, byte_len)
    }
}

/// Stores an error message in the thread-local `LAST_IO_ERROR` slot.
pub fn set_last_error(e: impl fmt::Display) {
    LAST_IO_ERROR.with(|cell| {
        *cell.borrow_mut() = Some(e.to_string());
    });
}

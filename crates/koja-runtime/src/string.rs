//! String and binary manipulation runtime functions.

use std::borrow::Cow;
use std::ffi::{CStr, c_char};
use std::ptr;
use std::slice;
use std::str;

use crate::parse_text::{ParseOutcome, parse_float_text, parse_int_text};
use crate::util::{BITS_PER_BYTE, alloc_koja_string, read_bit_length};

/// Decodes a NUL-terminated C string pointer into a string. Every Koja
/// `String` is valid UTF-8 by construction, so the borrowed path is taken
/// for all well-formed input; the lossy fallback exists only so malformed
/// bytes degrade to replacement characters instead of panicking across the
/// C-ABI.
///
/// # Safety
/// `ptr` must point to a valid NUL-terminated string.
unsafe fn cstr_str<'a>(ptr: *const u8) -> Cow<'a, str> {
    let cstr = unsafe { CStr::from_ptr(ptr as *const c_char) };
    String::from_utf8_lossy(cstr.to_bytes())
}

/// Attempts to parse a NUL-terminated string as a 64-bit float.
/// Returns a [`crate::parse_text`] code (`PARSE_OK` writes the
/// value through `out`); see [`parse_float_text`] for the
/// classification rules.
///
/// # Safety
/// `ptr` must point to a valid NUL-terminated string. `out` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_float_parse(ptr: *const u8, out: *mut f64) -> i64 {
    let s = unsafe { cstr_str(ptr) };
    let outcome = parse_float_text(s.trim());
    if let ParseOutcome::Ok(v) = outcome {
        unsafe { *out = v };
    }
    outcome.code()
}

/// Formats a Binary or Bits value as a literal-style string: `<<127, 0, 0, 1>>`.
///
/// # Safety
/// `ptr` must point to the payload of a valid Binary/Bits allocation with an 8-byte
/// length header at offset -8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_format_binary(ptr: *const u8, is_bits: i64) -> *const u8 {
    let bit_len = unsafe { read_bit_length(ptr) };
    if bit_len == 0 {
        return unsafe { alloc_koja_string(b"<<>>") };
    }

    let full_bytes = (bit_len / BITS_PER_BYTE as i64) as usize;
    let remainder_bits = (bit_len % BITS_PER_BYTE as i64) as usize;
    let total_bytes = full_bytes + if remainder_bits > 0 { 1 } else { 0 };

    let mut out = String::from("<<");
    for i in 0..total_bytes {
        if i > 0 {
            out.push_str(", ");
        }
        let byte = unsafe { *ptr.add(i) };
        if is_bits != 0 && remainder_bits > 0 && i == total_bytes - 1 {
            let mask = (1u16 << remainder_bits) - 1;
            let val = byte & (mask as u8);
            out.push_str(&format!("{}::{}", val, remainder_bits));
        } else {
            out.push_str(&format!("{}", byte));
        }
    }
    out.push_str(">>");

    unsafe { alloc_koja_string(out.as_bytes()) }
}

/// Attempts to parse a NUL-terminated string as a 64-bit signed integer.
/// Returns a [`crate::parse_text`] code (`PARSE_OK` writes the
/// value through `out`); see [`parse_int_text`] for the
/// classification rules.
///
/// # Safety
/// `ptr` must point to a valid NUL-terminated string. `out` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_int_parse(ptr: *const u8, out: *mut i64) -> i64 {
    let s = unsafe { cstr_str(ptr) };
    let outcome = parse_int_text(s.trim());
    if let ParseOutcome::Ok(v) = outcome {
        unsafe { *out = v };
    }
    outcome.code()
}

/// Returns a codepoint at `index`, or null if out of bounds.
///
/// # Safety
/// `ptr` must point to a valid NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_string_get(ptr: *const u8, index: i64) -> *const u8 {
    let s = unsafe { cstr_str(ptr) };
    let Some(ch) = s.chars().nth(index as usize) else {
        return ptr::null();
    };
    let mut buf = [0u8; 4];
    let encoded = ch.encode_utf8(&mut buf);
    unsafe { alloc_koja_string(encoded.as_bytes()) }
}

/// Returns the number of Unicode scalar values (codepoints) in a NUL-terminated
/// UTF-8 string.
///
/// # Safety
/// `ptr` must point to a valid NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_string_length(ptr: *const u8) -> i64 {
    let s = unsafe { cstr_str(ptr) };
    s.chars().count() as i64
}

/// Returns a substring spanning the inclusive codepoint range `[start, stop]`.
///
/// # Safety
/// `ptr` must point to a valid NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_string_slice(ptr: *const u8, start: i64, stop: i64) -> *const u8 {
    let s = unsafe { cstr_str(ptr) };
    let len = s.chars().count();

    let start = (start as usize).min(len);
    let stop = ((stop + 1) as usize).min(len);
    let stop = stop.max(start);

    let byte_start = s
        .char_indices()
        .nth(start)
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    let byte_end = if stop == len {
        s.len()
    } else {
        s.char_indices()
            .nth(stop)
            .map(|(i, _)| i)
            .unwrap_or(s.len())
    };
    let slice = &s[byte_start..byte_end];
    unsafe { alloc_koja_string(slice.as_bytes()) }
}

/// Validates that `len` bytes starting at `ptr` are valid UTF-8.
/// Returns 1 if valid, 0 otherwise.
///
/// # Safety
/// `ptr` must point to at least `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_utf8_validate(ptr: *const u8, len: u64) -> i64 {
    let slice = unsafe { slice::from_raw_parts(ptr, len as usize) };
    if str::from_utf8(slice).is_ok() { 1 } else { 0 }
}

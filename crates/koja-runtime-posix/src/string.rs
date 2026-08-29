//! String and binary manipulation runtime functions.

use std::ptr;
use std::slice;
use std::str;

use crate::parse_text::{ParseOutcome, parse_float_text, parse_int_text};
use crate::util::{
    BITS_PER_BYTE, alloc_binary, alloc_koja_string, read_bit_length, string_payload_bytes,
};

/// Borrows the complete UTF-8 payload of a Koja `String`.
///
/// # Safety
/// `ptr` must point to a valid Koja `String` payload.
unsafe fn payload_str<'a>(ptr: *const u8) -> &'a str {
    let bytes = unsafe { string_payload_bytes(ptr) };
    str::from_utf8(bytes).expect("Koja String payload must be valid UTF-8")
}

/// Byte offset of the first occurrence of `needle` in `haystack` at or
/// after byte offset `from`, or `None`. An empty needle matches at
/// `from`. Shared by `String.find` and `Binary.find` since both
/// payloads are plain byte sequences, and on valid UTF-8 a byte-level
/// match of a valid needle can only start on a codepoint boundary.
pub fn find_bytes(haystack: &[u8], needle: &[u8], from: i64) -> Option<usize> {
    let from = usize::try_from(from).ok()?;
    if from > haystack.len() {
        return None;
    }
    if needle.is_empty() {
        return Some(from);
    }
    let mut cursor = from;
    while cursor + needle.len() <= haystack.len() {
        let skip = haystack[cursor..haystack.len() - needle.len() + 1]
            .iter()
            .position(|&byte| byte == needle[0])?;
        let candidate = cursor + skip;
        if haystack[candidate..candidate + needle.len()] == *needle {
            return Some(candidate);
        }
        cursor = candidate + 1;
    }
    None
}

/// Converts a [`find_bytes`] result to the C ABI's `-1` sentinel.
fn find_bytes_sentinel(haystack: &[u8], needle: &[u8], from: i64) -> i64 {
    find_bytes(haystack, needle, from).map_or(-1, |offset| offset as i64)
}

/// Whether `offset` lands on a UTF-8 codepoint boundary of `bytes`,
/// assuming `bytes` is valid UTF-8. O(1), since a boundary is the end
/// of the buffer or any byte that is not a continuation byte. Shared
/// with `koja-ir-eval` so `slice_bytes` never re-validates the whole
/// payload per call, which would make callers quadratic again.
pub fn is_utf8_boundary(bytes: &[u8], offset: usize) -> bool {
    offset >= bytes.len() || (bytes[offset] & 0xC0) != 0x80
}

/// Attempts to parse a Koja string as a 64-bit float.
/// Returns a [`crate::parse_text`] code (`PARSE_OK` writes the
/// value through `out`). See [`parse_float_text`] for the
/// classification rules.
///
/// # Safety
/// `ptr` must point to a valid Koja `String` payload. `out` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_float_parse(ptr: *const u8, out: *mut f64) -> i64 {
    let s = unsafe { payload_str(ptr) };
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

/// Attempts to parse a Koja string as a 64-bit signed integer.
/// Returns a [`crate::parse_text`] code (`PARSE_OK` writes the
/// value through `out`). See [`parse_int_text`] for the
/// classification rules.
///
/// # Safety
/// `ptr` must point to a valid Koja `String` payload. `out` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_int_parse(ptr: *const u8, out: *mut i64) -> i64 {
    let s = unsafe { payload_str(ptr) };
    let outcome = parse_int_text(s.trim());
    if let ParseOutcome::Ok(v) = outcome {
        unsafe { *out = v };
    }
    outcome.code()
}

/// Returns the byte offset of the first occurrence of `needle` at or
/// after byte offset `from`, or -1 when absent. See [`find_bytes`].
///
/// # Safety
/// `ptr` and `needle` must point to valid Koja `String` payloads.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_string_find(ptr: *const u8, needle: *const u8, from: i64) -> i64 {
    let haystack = unsafe { string_payload_bytes(ptr) };
    let needle = unsafe { string_payload_bytes(needle) };
    find_bytes_sentinel(haystack, needle, from)
}

/// Returns the byte offset of the first occurrence of `needle` at or
/// after byte offset `from`, or -1 when absent. See [`find_bytes`].
///
/// # Safety
/// `ptr` and `needle` must point to valid Binary payloads.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_binary_find(ptr: *const u8, needle: *const u8, from: i64) -> i64 {
    let haystack = unsafe { string_payload_bytes(ptr) };
    let needle = unsafe { string_payload_bytes(needle) };
    find_bytes_sentinel(haystack, needle, from)
}

/// Returns a codepoint at `index`, or null if out of bounds.
///
/// # Safety
/// `ptr` must point to a valid Koja `String` payload.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_string_get(ptr: *const u8, index: i64) -> *const u8 {
    let s = unsafe { payload_str(ptr) };
    let Some(ch) = s.chars().nth(index as usize) else {
        return ptr::null();
    };
    let mut buf = [0u8; 4];
    let encoded = ch.encode_utf8(&mut buf);
    unsafe { alloc_koja_string(encoded.as_bytes()) }
}

/// Returns the number of Unicode scalar values in a Koja string.
///
/// # Safety
/// `ptr` must point to a valid Koja `String` payload.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_string_length(ptr: *const u8) -> i64 {
    let s = unsafe { payload_str(ptr) };
    s.chars().count() as i64
}

/// Returns the character at a UTF-8 byte cursor and writes the next cursor.
///
/// # Safety
/// `ptr` must point to a valid Koja `String` payload. `next` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_string_next(
    ptr: *const u8,
    cursor: i64,
    next: *mut i64,
) -> *const u8 {
    let s = unsafe { payload_str(ptr) };
    let Some(offset) = usize::try_from(cursor).ok() else {
        return ptr::null();
    };
    let Some(suffix) = s.get(offset..) else {
        return ptr::null();
    };
    let Some(character) = suffix.chars().next() else {
        return ptr::null();
    };
    unsafe {
        *next = (offset + character.len_utf8()) as i64;
    }
    let mut bytes = [0; 4];
    unsafe { alloc_koja_string(character.encode_utf8(&mut bytes).as_bytes()) }
}

/// Compares the complete byte payloads of two Koja strings.
///
/// # Safety
/// Both pointers must point to valid Koja `String` payloads.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_string_eq(lhs: *const u8, rhs: *const u8) -> i64 {
    let lhs = unsafe { string_payload_bytes(lhs) };
    let rhs = unsafe { string_payload_bytes(rhs) };
    i64::from(lhs == rhs)
}

/// Returns whether a Koja string contains an interior NUL byte.
///
/// # Safety
/// `ptr` must point to a valid Koja `String` payload.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_string_contains_nul(ptr: *const u8) -> i64 {
    i64::from(unsafe { string_payload_bytes(ptr) }.contains(&0))
}

/// Returns a substring spanning the inclusive codepoint range `[start, stop]`.
/// Out-of-bounds endpoints clamp to the string boundaries. One forward
/// scan bounded by `stop`, so the cost is O(stop) rather than O(length).
///
/// # Safety
/// `ptr` must point to a valid Koja `String` payload.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_string_slice(ptr: *const u8, start: i64, stop: i64) -> *const u8 {
    let s = unsafe { payload_str(ptr) };
    let (byte_start, byte_end) = codepoint_range_to_bytes(s, start, stop);
    unsafe { alloc_koja_string(&s.as_bytes()[byte_start..byte_end]) }
}

/// Resolves an inclusive codepoint range to a byte range in one
/// forward scan that stops at the range end. Clamps both endpoints to
/// the string boundaries.
pub fn codepoint_range_to_bytes(s: &str, start: i64, stop: i64) -> (usize, usize) {
    if stop < start || stop < 0 {
        return (0, 0);
    }
    let start = start.max(0) as usize;
    let stop_exclusive = stop as usize + 1;
    let mut byte_start = s.len();
    let mut byte_end = s.len();
    for (codepoint_index, (byte_offset, _)) in s.char_indices().enumerate() {
        if codepoint_index == start {
            byte_start = byte_offset;
        }
        if codepoint_index == stop_exclusive {
            byte_end = byte_offset;
            break;
        }
    }
    (byte_start, byte_end)
}

/// Returns a copy of the byte range `[start, stop)`. Endpoints clamp
/// to the string's byte length and must land on codepoint boundaries.
/// Callers in the stdlib's search family only pass offsets produced by
/// `find` and `byte_length`, which are boundaries by construction.
///
/// # Safety
/// `ptr` must point to a valid Koja `String` payload.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_string_slice_bytes(
    ptr: *const u8,
    start: i64,
    stop: i64,
) -> *const u8 {
    // Raw bytes rather than `payload_str`, whose full UTF-8 validation
    // per call would make split-style loops quadratic again.
    let bytes = unsafe { string_payload_bytes(ptr) };
    let start = (start.max(0) as usize).min(bytes.len());
    let stop = (stop.max(0) as usize).min(bytes.len()).max(start);
    // Abort on a caller invariant violation instead of forging an
    // invalid String.
    assert!(
        is_utf8_boundary(bytes, start) && is_utf8_boundary(bytes, stop),
        "String.slice_bytes offsets must land on codepoint boundaries",
    );
    unsafe { alloc_koja_string(&bytes[start..stop]) }
}

/// Returns a new `Binary` spanning the inclusive byte range
/// `[start, stop]`. Out-of-bounds endpoints clamp to the binary's
/// boundaries.
///
/// # Safety
/// `payload` must point to a valid Binary payload with its
/// `[i64 rc][i64 bit_length]` header.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_binary_slice(payload: *const u8, start: i64, stop: i64) -> *mut u8 {
    let len = (unsafe { read_bit_length(payload) } / BITS_PER_BYTE as i64) as usize;
    let start = (start.max(0) as usize).min(len);
    let stop = ((stop + 1).max(0) as usize).min(len).max(start);
    let bytes = unsafe { slice::from_raw_parts(payload, len) };
    alloc_binary(&bytes[start..stop])
}

/// Validates that `len` bytes starting at `ptr` are valid UTF-8.
/// Returns 1 if valid, 0 otherwise.
///
/// # Safety
/// `ptr` must point to at least `len` readable bytes unless `len` is zero.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_utf8_validate(ptr: *const u8, len: i64) -> i64 {
    if len == 0 {
        return 1;
    }
    if len < 0 || ptr.is_null() {
        return 0;
    }
    let slice = unsafe { slice::from_raw_parts(ptr, len as usize) };
    if str::from_utf8(slice).is_ok() { 1 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory;
    use crate::util::BLOCK_HEADER_SIZE;

    #[test]
    fn string_next_advances_by_utf8_bytes() {
        let input = unsafe { alloc_koja_string("éa".as_bytes()) };
        let mut next = -1;
        let character = unsafe { koja_string_next(input, 0, &mut next) };

        assert_eq!(unsafe { string_payload_bytes(character) }, "é".as_bytes());
        assert_eq!(next, 2);

        unsafe {
            memory::free((input as *mut u8).sub(BLOCK_HEADER_SIZE));
            memory::free((character as *mut u8).sub(BLOCK_HEADER_SIZE));
        }
    }

    #[test]
    fn find_bytes_locates_matches_and_respects_from() {
        let haystack = b"one,two,,three";
        assert_eq!(find_bytes(haystack, b",", 0), Some(3));
        assert_eq!(find_bytes(haystack, b",", 4), Some(7));
        assert_eq!(find_bytes(haystack, b",,", 0), Some(7));
        assert_eq!(find_bytes(haystack, b"three", 0), Some(9));
        assert_eq!(find_bytes(haystack, b"four", 0), None);
        assert_eq!(find_bytes(haystack, b",", 9), None);
    }

    #[test]
    fn find_bytes_handles_empty_and_out_of_range() {
        assert_eq!(find_bytes(b"abc", b"", 1), Some(1));
        assert_eq!(find_bytes(b"abc", b"", 3), Some(3));
        assert_eq!(find_bytes(b"abc", b"", 4), None);
        assert_eq!(find_bytes(b"abc", b"a", -1), None);
        assert_eq!(find_bytes(b"", b"a", 0), None);
        assert_eq!(find_bytes(b"ab", b"abc", 0), None);
    }

    #[test]
    fn find_bytes_works_on_arbitrary_bytes() {
        let haystack = [0xff, 0x00, 0xfe, 0x00, 0xfe];
        assert_eq!(find_bytes(&haystack, &[0x00, 0xfe], 0), Some(1));
        assert_eq!(find_bytes(&haystack, &[0x00, 0xfe], 2), Some(3));
    }

    #[test]
    fn codepoint_range_clamps_and_handles_multibyte() {
        // In "héllo", h is 1 byte and é is 2.
        let s = "héllo";
        assert_eq!(codepoint_range_to_bytes(s, 0, 1), (0, 3));
        assert_eq!(codepoint_range_to_bytes(s, 1, 2), (1, 4));
        assert_eq!(codepoint_range_to_bytes(s, 2, 100), (3, 6));
        assert_eq!(codepoint_range_to_bytes(s, -3, 0), (0, 1));
        assert_eq!(codepoint_range_to_bytes(s, 3, 1), (0, 0));
        assert_eq!(codepoint_range_to_bytes(s, 9, 12), (6, 6));
    }

    #[test]
    fn string_slice_bytes_copies_the_byte_range() {
        let input = unsafe { alloc_koja_string("héllo".as_bytes()) };
        let piece = unsafe { koja_string_slice_bytes(input, 1, 3) };
        assert_eq!(unsafe { string_payload_bytes(piece) }, "é".as_bytes());

        let clamped = unsafe { koja_string_slice_bytes(input, 3, 99) };
        assert_eq!(unsafe { string_payload_bytes(clamped) }, "llo".as_bytes());

        unsafe {
            memory::free((input as *mut u8).sub(BLOCK_HEADER_SIZE));
            memory::free((piece as *mut u8).sub(BLOCK_HEADER_SIZE));
            memory::free((clamped as *mut u8).sub(BLOCK_HEADER_SIZE));
        }
    }

    #[test]
    fn string_next_returns_null_for_invalid_cursors() {
        let input = unsafe { alloc_koja_string("é".as_bytes()) };
        let mut next = -1;

        assert!(unsafe { koja_string_next(input, -1, &mut next) }.is_null());
        assert!(unsafe { koja_string_next(input, 1, &mut next) }.is_null());
        assert!(unsafe { koja_string_next(input, 2, &mut next) }.is_null());
        assert_eq!(next, -1);

        unsafe {
            memory::free((input as *mut u8).sub(BLOCK_HEADER_SIZE));
        }
    }
}

//! Shared helpers used across multiple runtime modules.

use std::cell::RefCell;
use std::fmt;
use std::ptr;

use crate::memory;

/// Total bytes of header prepended to every rc-managed leaf heap
/// payload (`String` / `Binary` / `Bits`): `[i64 rc][i64 bit_length]`
/// before the payload, so the payload sits `BLOCK_HEADER_SIZE` bytes
/// past the block base. The runtime-side source of truth for the heap
/// header ABI; mirrored on the codegen side by `koja-ir-llvm`'s
/// `emit::heap_layout::HEADER_BYTES`. The two are an API contract kept
/// in sync by convention.
pub const BLOCK_HEADER_SIZE: usize = 16;
/// Distance in bytes from a payload pointer back to its `i64
/// bit_length` word. The rc word sits a further `LENGTH_OFFSET` before
/// that (i.e. at the block base, `BLOCK_HEADER_SIZE` before payload).
pub const LENGTH_OFFSET: usize = 8;
/// Number of bits in a byte, used for bit-length / byte-length conversions.
pub const BITS_PER_BYTE: usize = 8;

/// Reads the `i64 bit_length` header sitting `LENGTH_OFFSET` bytes
/// before a heap payload pointer.
///
/// # Safety
/// `payload` must point at the body of a heap-emitted string /
/// `Binary` / `Bits` (the byte right after the header). Any other
/// pointer is undefined behavior.
pub unsafe fn read_bit_length(payload: *const u8) -> i64 {
    unsafe { *payload.sub(LENGTH_OFFSET).cast::<i64>() }
}

/// Initialize a freshly-allocated leaf heap block: write `rc = 1` at
/// the block base, the `bit_length` word `LENGTH_OFFSET` after it, and
/// return the payload pointer (`base + BLOCK_HEADER_SIZE`). The single
/// place the `[i64 rc][i64 bit_length]` header is stamped on the
/// runtime side.
///
/// # Safety
/// `base` must point at the start of an allocation of at least
/// `BLOCK_HEADER_SIZE + payload_bytes` bytes from [`memory::alloc`].
pub unsafe fn write_block_header(base: *mut u8, bit_length: i64) -> *mut u8 {
    unsafe {
        *base.cast::<i64>() = 1;
        *base.add(LENGTH_OFFSET).cast::<i64>() = bit_length;
        base.add(BLOCK_HEADER_SIZE)
    }
}

/// Increment the refcount of an rc-managed leaf heap block. `base`
/// points at the block base (the `i64 rc` word). Immortal blocks —
/// statically-allocated rodata payloads, stamped with a negative
/// sentinel rc by codegen — are left untouched (`rc < 0`). Null is a
/// no-op.
///
/// # Safety
/// `base` must be null or the base of a live rc-managed block.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_rc_inc(base: *mut u8) {
    if base.is_null() {
        return;
    }
    unsafe {
        let rc = *base.cast::<i64>();
        if rc < 0 {
            return;
        }
        *base.cast::<i64>() = rc + 1;
    }
}

/// Decrement the refcount of an rc-managed leaf heap block, freeing it
/// when the count reaches zero. `base` points at the block base (the
/// `i64 rc` word). Immortal blocks (`rc < 0`) are left untouched. Null
/// is a no-op.
///
/// # Safety
/// `base` must be null or the base of a live rc-managed block that has
/// not already been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_rc_dec(base: *mut u8) {
    if base.is_null() {
        return;
    }
    unsafe {
        let rc = *base.cast::<i64>();
        if rc < 0 {
            return;
        }
        let remaining = rc - 1;
        if remaining == 0 {
            memory::free(base);
        } else {
            *base.cast::<i64>() = remaining;
        }
    }
}

/// Borrows the bytes of a heap-emitted Koja string / `Binary` body
/// by reading the `bit_length` header and slicing the corresponding
/// byte count.
///
/// # Safety
/// Same contract as [`read_bit_length`].
pub unsafe fn string_payload_bytes<'a>(payload: *const u8) -> &'a [u8] {
    let byte_length = (unsafe { read_bit_length(payload) } / BITS_PER_BYTE as i64) as usize;
    unsafe { std::slice::from_raw_parts(payload, byte_length) }
}

thread_local! {
    static LAST_IO_ERROR: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Allocates a Binary value with the given bytes (8-byte length header + data).
/// Returns a pointer to the payload (past the header).
pub fn alloc_binary(data: &[u8]) -> *mut u8 {
    let total = BLOCK_HEADER_SIZE + data.len();
    let base = memory::alloc(total);
    let bit_len = (data.len() as i64) * BITS_PER_BYTE as i64;
    unsafe {
        let payload = write_block_header(base, bit_len);
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
        let base = memory::alloc(BLOCK_HEADER_SIZE + byte_len + 1);
        let bit_len = (byte_len as i64) * BITS_PER_BYTE as i64;
        let payload = write_block_header(base, bit_len);
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
    let buf = memory::alloc(count * std::mem::size_of::<*const u8>()) as *mut *const u8;
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

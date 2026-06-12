//! The rc-prefixed heap-block ABI shared with the runtime:
//! `[i64 rc][i64 bit_length][payload…]` backs every Koja `String` /
//! `Binary` payload that crosses the C boundary. This module is the
//! single eval-side owner of the layout constants and the
//! alloc/copy/free helpers — `intrinsics/binary.rs` (`Binary.ptr`),
//! `intrinsics/cptr.rs` (`CPtr.to_string`), and
//! `intrinsics/socket.rs` (resolve / recv_from buffers) all marshal
//! through here.
//!
//! Allocation and free route through the runtime's `koja_alloc` /
//! `koja_free` symbols rather than bare libc so blocks minted by
//! eval and blocks minted by the runtime are interchangeable,
//! including under the runtime's live-block accounting.

use std::ptr;
use std::slice;

/// Distance in bytes from a payload pointer back to its block base
/// (the `i64 rc` word). API contract: MUST equal
/// [`koja_runtime`]'s `util::BLOCK_HEADER_SIZE`.
pub(crate) const BLOCK_HEADER_SIZE: usize = 16;
/// Distance in bytes from a payload pointer back to its `i64
/// bit_length` word. API contract: MUST equal [`koja_runtime`]'s
/// `util::LENGTH_OFFSET`.
pub(crate) const LENGTH_OFFSET: usize = 8;
/// `i64 rc` for a freshly allocated (mortal) block.
const RC_INITIAL: i64 = 1;
const BITS_PER_BYTE: usize = 8;

unsafe extern "C" {
    fn koja_alloc(size: usize) -> *mut u8;
    fn koja_free(ptr: *mut u8);
}

/// Copy `data` into a fresh block and return the payload pointer.
/// Mirrors the runtime's `alloc_koja_string`: the payload is
/// nul-terminated past `data.len()` so C consumers that walk to the
/// terminator (e.g. `koja_socket_send_to`) stay in bounds, and empty
/// inputs still get a real, non-null allocation — the same shape the
/// runtime hands out.
pub(crate) fn alloc_block(data: &[u8]) -> *mut u8 {
    let base = unsafe { koja_alloc(BLOCK_HEADER_SIZE + data.len() + 1) };
    unsafe {
        *(base as *mut i64) = RC_INITIAL;
        *(base.add(LENGTH_OFFSET) as *mut i64) = (data.len() * BITS_PER_BYTE) as i64;
        let payload = base.add(BLOCK_HEADER_SIZE);
        ptr::copy_nonoverlapping(data.as_ptr(), payload, data.len());
        *payload.add(data.len()) = 0;
        payload
    }
}

/// The `i64 bit_length` header of the block backing `payload`.
pub(crate) fn read_bit_length(payload: *const u8) -> i64 {
    unsafe { *(payload.sub(LENGTH_OFFSET) as *const i64) }
}

/// Copy the payload bytes of a block into an owned vec. A null
/// `payload` or a negative header length yields empty.
pub(crate) fn block_bytes(payload: *const u8) -> Vec<u8> {
    if payload.is_null() {
        return Vec::new();
    }
    let byte_length = (read_bit_length(payload).max(0) as usize) / BITS_PER_BYTE;
    unsafe { slice::from_raw_parts(payload, byte_length) }.to_vec()
}

/// Release the block backing `payload` (no-op for null).
pub(crate) fn free_block(payload: *mut u8) {
    if !payload.is_null() {
        unsafe { koja_free(payload.sub(BLOCK_HEADER_SIZE)) };
    }
}

/// [`block_bytes`] + [`free_block`]: copy the payload out and
/// release the block. For runtime-allocated result buffers whose
/// ownership transfers to eval.
pub(crate) fn take_block_bytes(payload: *mut u8) -> Vec<u8> {
    let bytes = block_bytes(payload);
    free_block(payload);
    bytes
}

/// Release a raw (header-less) runtime allocation, e.g. the
/// pointer-array buffers `koja_socket_resolve` / `koja_socket_recv_from`
/// return. The pointer itself is the allocation base.
pub(crate) fn free_raw_buffer(buffer: *mut u8) {
    if !buffer.is_null() {
        unsafe { koja_free(buffer) };
    }
}

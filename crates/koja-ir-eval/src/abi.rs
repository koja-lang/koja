//! The rc-prefixed heap-block ABI shared with the runtime:
//! `[i64 rc][i64 bit_length][payload…]` backs every Koja `String` /
//! `Binary` payload that crosses the C boundary. This module is the
//! single eval-side owner of the layout constants and the
//! copy/free helpers used by runtime block adoption and socket
//! result buffers.
//!
//! Freeing routes through the runtime's `koja_free` symbol so live
//! block accounting remains balanced.

use std::slice;

/// Distance in bytes from a payload pointer back to its block base
/// (the `i64 rc` word). API contract: MUST equal
/// [`koja_runtime`]'s `util::BLOCK_HEADER_SIZE`.
pub(crate) const BLOCK_HEADER_SIZE: usize = 16;
/// Distance in bytes from a payload pointer back to its `i64
/// bit_length` word. API contract: MUST equal [`koja_runtime`]'s
/// `util::LENGTH_OFFSET`.
pub(crate) const LENGTH_OFFSET: usize = 8;
const BITS_PER_BYTE: usize = 8;

unsafe extern "C" {
    fn koja_free(ptr: *mut u8);
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

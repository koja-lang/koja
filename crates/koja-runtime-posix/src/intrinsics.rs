//! C-ABI runtime helpers called from LLVM-emitted IR. The LLVM
//! backend names each of these as an external declaration and emits
//! direct calls; the eval interpreter mirrors each helper's
//! behavior in pure Rust so the two backends produce byte-identical
//! results.
//!
//! - [`__koja_print_string`] — the runtime body of the
//!   [`Global.print`](../../koja-ir-llvm/src/intrinsics/print.rs)
//!   intrinsic. Writes the bytes of a heap string followed by a
//!   newline.
//! - [`__koja_panic`] — the runtime body of `Kernel.panic`.
//!   Routes the message through the panic backtrace formatter
//!   (`** (panic) <message>` + filtered stack), then aborts.
//! - [`__koja_concat_bits`] / [`__koja_pack_bits`] — helpers for
//!   the LLVM emitter's bit-packing paths; emitting the sub-byte
//!   alignment logic is far cleaner in Rust than in LLVM IR.

use std::io::{self, Write};

use crate::memory;
use crate::panic::{PanicOrigin, abort_with_diagnostic};
use crate::util::{BLOCK_HEADER_SIZE, read_bit_length, string_payload_bytes, write_block_header};

/// Concatenate two `Bits` values produced by the LLVM backend.
/// Reads `bit_length` from each operand's `payload - 8` header,
/// allocates a fresh `[i64 bit_length][ceil((L+R)/8) bytes]` heap
/// block, copies lhs verbatim, and bit-shifts rhs to land at the
/// lhs trailing partial byte. Returns the new payload pointer
/// (8 bytes past the block base) so the caller drops with the same
/// `payload - 8` recipe used for `String` / `Binary`.
///
/// Sub-byte alignment is far cleaner in Rust than LLVM IR, so the
/// LLVM backend's `IRInstruction::Concat` arm for
/// `ConcatKind::Bits` calls this helper instead of inlining.
///
/// # Safety
///
/// Both `lhs` and `rhs` must point at heap-payload pointers
/// (i.e. 8 bytes past their `i64 bit_length` headers). Calling
/// with any other pointer is undefined behavior.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __koja_concat_bits(lhs: *const u8, rhs: *const u8) -> *const u8 {
    let l_bits = unsafe { read_bit_length(lhs) } as u64;
    let r_bits = unsafe { read_bit_length(rhs) } as u64;
    let total_bits = l_bits + r_bits;
    let total_bytes = total_bits.div_ceil(8) as usize;
    let block_size = BLOCK_HEADER_SIZE + total_bytes;

    let block = memory::alloc(block_size);
    let payload = unsafe { write_block_header(block, total_bits as i64) };
    if total_bytes > 0 {
        unsafe { std::ptr::write_bytes(payload, 0, total_bytes) };
    }

    let l_bytes = (l_bits / 8) as usize;
    let l_trailing = (l_bits % 8) as u32;
    if l_bytes > 0 {
        unsafe { std::ptr::copy_nonoverlapping(lhs, payload, l_bytes) };
    }
    if l_trailing > 0 {
        // The lhs's trailing partial byte sits at `payload[l_bytes]`;
        // copy it (low bits already zero per the bit-packing invariant).
        unsafe {
            *payload.add(l_bytes) = *lhs.add(l_bytes);
        }
    }

    if r_bits > 0 {
        unsafe { append_bits_into(payload, l_bits, rhs, r_bits) };
    }

    payload as *const u8
}

/// Append `length` bits from `src` (left-aligned, low-bit zero pad
/// in the trailing partial byte) into `dest` starting at bit
/// offset `start_bit`. Helper for [`__koja_concat_bits`];
/// mirrors the eval interpreter's `append_bits` so eval / native
/// produce byte-identical results for the same input.
///
/// # Safety
///
/// `dest` must point at a writable byte buffer with at least
/// `ceil((start_bit + length) / 8)` bytes. `src` must point at a
/// readable buffer with at least `ceil(length / 8)` bytes.
unsafe fn append_bits_into(dest: *mut u8, start_bit: u64, src: *const u8, length: u64) {
    if length == 0 {
        return;
    }
    let shift = (start_bit % 8) as u32;
    let dest_byte_start = (start_bit / 8) as usize;
    if shift == 0 {
        let src_bytes = length.div_ceil(8) as usize;
        unsafe {
            std::ptr::copy_nonoverlapping(src, dest.add(dest_byte_start), src_bytes);
        }
        return;
    }
    let src_bytes = length.div_ceil(8) as usize;
    let mut remaining = length;
    for idx in 0..src_bytes {
        let byte = unsafe { *src.add(idx) };
        unsafe {
            *dest.add(dest_byte_start + idx) |= byte >> shift;
        }
        let next_offset = dest_byte_start + idx + 1;
        let bits_in_this_byte = remaining.min(8);
        if bits_in_this_byte + shift as u64 > 8 {
            unsafe {
                *dest.add(next_offset) |= byte << (8 - shift);
            }
        }
        if remaining > 8 {
            remaining -= 8;
        } else {
            remaining = 0;
        }
    }
}

/// Pack the low `width` bits of `value` (MSB-first) into `payload`
/// at bit offset `bit_offset`. Used by the LLVM backend's
/// `IRInstruction::BinaryConstruct` lowering to handle sub-byte
/// segment widths — the bit-shift loop in LLVM IR is far messier
/// than the same logic in Rust, so the LLVM emitter delegates here.
///
/// `payload` is `or`-merged: the `BinaryConstruct` lowerer
/// pre-zeros the buffer once via `memset(0)`, so the helper writes
/// only the `1`-bits and adjacent segments sharing a byte don't
/// clobber each other. Mirrors the eval interpreter's
/// `pack_bits_into` so eval and native produce byte-identical
/// results for the same input.
///
/// # Safety
///
/// `payload` must point at a writable byte buffer with at least
/// `ceil((bit_offset + width) / 8)` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __koja_pack_bits(
    payload: *mut u8,
    value: i64,
    width: u8,
    bit_offset: i64,
) {
    let width = width as u64;
    let bit_offset = bit_offset as u64;
    let value = value as u64;
    if width == 0 {
        return;
    }
    for i in 0..width {
        let bit = ((value >> (width - 1 - i)) & 1) as u8;
        if bit == 0 {
            continue;
        }
        let bit_pos = bit_offset + i;
        let byte = (bit_pos / 8) as usize;
        let bit_in_byte = 7 - (bit_pos % 8) as u32;
        unsafe {
            *payload.add(byte) |= 1 << bit_in_byte;
        }
    }
}

/// Abort the process with a diagnostic message. Paired with the
/// LLVM backend's `Kernel.panic` emitter — the emitter passes the
/// `String` payload pointer (8 bytes past the length header) and
/// trails the call with `unreachable`, so this helper never has
/// to return. Reads the `i64` bit length from `payload-8`, then
/// routes the message through [`crate::panic::abort_with_diagnostic`]
/// — printing `** (panic) <message>` followed by a filtered,
/// symbolicated backtrace — before aborting.
///
/// # Safety
///
/// `payload` must point at the body of a heap-emitted string
/// (i.e. the byte right after the `i64 bit_length` header).
/// Calling with any other pointer is undefined behavior.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __koja_panic(payload: *const u8) -> ! {
    let bytes = unsafe { string_payload_bytes(payload) };
    let message = String::from_utf8_lossy(bytes);
    abort_with_diagnostic(PanicOrigin::User, &message);
}

/// Print a `String`-flavored body value followed by a newline.
/// Reads the `i64` bit-length 8 bytes before `payload` (the header
/// layout shared with `Binary` / `Bits`; see `IRType::String`) and
/// writes that many UTF-8 bytes to stdout.
///
/// # Safety
///
/// `payload` must point at the body of a heap-emitted string
/// global (`emit_const_string` in `koja-ir-llvm`), i.e. the byte
/// right after the `i64 bit_length` header. Calling with any
/// other pointer is undefined behavior.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __koja_print_string(payload: *const u8) {
    let bytes = unsafe { string_payload_bytes(payload) };
    let mut stdout = io::stdout().lock();
    let _ = stdout.write_all(bytes);
    let _ = stdout.write_all(b"\n");
}

//! Runtime helpers for the alpha LLVM backend that don't have a
//! natural home in `scheduler.rs` (concurrency) or `format.rs`
//! (number → string rendering):
//!
//! - [`__expo_alpha_print_string`] — the runtime body of the
//!   [`Global.print`](../../expo-alpha-ir-llvm/src/intrinsics/print.rs)
//!   intrinsic. Writes the bytes of an alpha-emitted heap string
//!   followed by a newline.
//! - [`__expo_alpha_panic`] — the runtime body of `Kernel.panic`.
//!   Writes `panic: <message>\n` to stderr and aborts.
//! - [`__expo_alpha_concat_bits`] / [`__expo_alpha_pack_bits`] —
//!   helpers for the LLVM emitter's bit-packing paths; emitting
//!   the sub-byte alignment logic is far cleaner in Rust than in
//!   LLVM IR.

use std::io::{self, Write};

/// Concatenate two `Bits` values produced by alpha LLVM. Reads
/// `bit_length` from each operand's `payload - 8` header,
/// allocates a fresh `[i64 bit_length][ceil((L+R)/8) bytes]` heap
/// block, copies lhs verbatim, and bit-shifts rhs to land at the
/// lhs trailing partial byte. Returns the new payload pointer
/// (8 bytes past the block base) so the caller drops with the same
/// `payload - 8` recipe used for `String` / `Binary`.
///
/// Sub-byte alignment is far cleaner in Rust than LLVM IR, so the
/// alpha LLVM backend's `IRInstruction::Concat` arm for
/// `ConcatKind::Bits` calls this helper instead of inlining.
///
/// # Safety
///
/// Both `lhs` and `rhs` must point at alpha-emitted heap-payload
/// pointers (i.e. 8 bytes past their `i64 bit_length` headers).
/// Calling with any other pointer is undefined behavior.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __expo_alpha_concat_bits(lhs: *const u8, rhs: *const u8) -> *const u8 {
    let l_bits = unsafe { *(lhs.offset(-8).cast::<i64>()) } as u64;
    let r_bits = unsafe { *(rhs.offset(-8).cast::<i64>()) } as u64;
    let total_bits = l_bits + r_bits;
    let total_bytes = total_bits.div_ceil(8) as usize;
    let block_size = 8 + total_bytes;

    let block = unsafe { libc::malloc(block_size) } as *mut u8;
    if block.is_null() {
        std::process::abort();
    }
    unsafe {
        *(block.cast::<i64>()) = total_bits as i64;
    }
    let payload = unsafe { block.add(8) };
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
        // copy it (low bits already zero per the alpha invariant).
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
/// offset `start_bit`. Helper for [`__expo_alpha_concat_bits`];
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
/// at bit offset `bit_offset`. Used by alpha LLVM's
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
pub unsafe extern "C" fn __expo_alpha_pack_bits(
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
/// alpha LLVM backend's `Kernel.panic` emitter — the emitter passes
/// the `String` payload pointer (8 bytes past the v1 length header)
/// and trails the call with `unreachable`, so this helper never has
/// to return. Reads the `i64` bit length from `payload-8`, writes
/// `panic: <message>\n` to stderr, then `process::abort`s.
///
/// # Safety
///
/// `payload` must point at the body of an alpha-emitted string
/// global (i.e. the byte right after the `i64 bit_length` header).
/// Calling with any other pointer is undefined behavior.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __expo_alpha_panic(payload: *const u8) -> ! {
    let header = unsafe { payload.offset(-8).cast::<i64>() };
    let bit_length = unsafe { *header };
    let byte_length = (bit_length / 8) as usize;
    let bytes = unsafe { std::slice::from_raw_parts(payload, byte_length) };
    let mut stderr = io::stderr().lock();
    let _ = stderr.write_all(b"panic: ");
    let _ = stderr.write_all(bytes);
    let _ = stderr.write_all(b"\n");
    std::process::abort();
}

/// Print a `String`-flavored body value followed by a newline.
/// Reads the `i64` bit-length 8 bytes before `payload` (the v1 header
/// layout shared with `Binary` / `Bits`; see `IRType::String`) and
/// writes that many UTF-8 bytes to stdout.
///
/// # Safety
///
/// `payload` must point at the body of an alpha-emitted string global
/// (`emit_const_string` in `expo-alpha-ir-llvm`), i.e. the byte right
/// after the `i64 bit_length` header. Calling with any other pointer
/// is undefined behavior.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __expo_alpha_print_string(payload: *const u8) {
    let header = unsafe { payload.offset(-8).cast::<i64>() };
    let bit_length = unsafe { *header };
    let byte_length = (bit_length / 8) as usize;
    let bytes = unsafe { std::slice::from_raw_parts(payload, byte_length) };
    let mut stdout = io::stdout().lock();
    let _ = stdout.write_all(bytes);
    let _ = stdout.write_all(b"\n");
}

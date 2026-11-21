//! Shared helpers used across multiple runtime modules.

use std::io;
use std::ptr;

use crate::ffi::{EBADF, last_os_error};
use crate::memory;
use crate::panic::exit_before_entry;

/// Total bytes of header prepended to every rc-managed leaf heap
/// payload (`String` / `Binary` / `Bits`): `[i64 rc][i64 bit_length]`
/// before the payload, so the payload sits `BLOCK_HEADER_SIZE` bytes
/// past the block base. The runtime-side source of truth for the heap
/// header ABI, mirrored on the codegen side by `koja-ir-llvm`'s
/// `emit::heap_layout::HEADER_BYTES`. The two are an API contract kept
/// in sync by convention.
pub const BLOCK_HEADER_SIZE: usize = 16;
/// Distance in bytes from a payload pointer back to its `i64
/// bit_length` word. The rc word sits a further `LENGTH_OFFSET` before
/// that (i.e. at the block base, `BLOCK_HEADER_SIZE` before payload).
///
/// A closure env block carries a 40-byte header instead: `[i64
/// rc][ptr drop_fn][ptr copy_fn][i64 site_id][ptr eq_fn]`. `drop_fn`
/// (`LENGTH_OFFSET` bytes past the base) is the address of the
/// closure's capture-release glue (or null when no capture is
/// heap-managed). `copy_fn` ([`COPY_FN_OFFSET`] bytes past the base)
/// is the address of its env deep-copy glue (or null when the closure
/// was built outside lowering and can never cross a process
/// boundary). `site_id` and `eq_fn` are only read by compiled
/// `ClosureEquals` code. Captures follow the header. The base pointer
/// is the env pointer itself, so [`koja_rc_inc`] /
/// [`koja_closure_rc_dec`] operate on the env directly. Mirrored
/// codegen-side by `koja-ir-llvm`'s `CLOSURE_ENV_HEADER_FIELDS`.
pub const LENGTH_OFFSET: usize = 8;
/// Distance in bytes from a closure env base to its `ptr copy_fn`
/// header word (see the closure env header note on
/// [`LENGTH_OFFSET`]).
pub const COPY_FN_OFFSET: usize = 16;
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

/// Smallest allocation an in-place growth reallocates to, so short
/// builders stop reallocating after the first step.
const MIN_GROWN_BLOCK: usize = 64;

/// Make room for `extra_bytes` past the current payload of a leaf
/// block that this caller solely owns. Capacity comes from the
/// allocator's slack ([`memory::usable_size`]), and a short block
/// reallocates to double the needed size, so repeated growth is
/// amortized O(1). Returns the possibly moved payload, or `None` when
/// the block is shared (`rc > 1`) or immortal (`rc < 0`) and the
/// caller must copy and release instead. `bit_length` is left for the
/// caller to update.
///
/// # Safety
/// `payload` must point at the body of a live heap leaf block.
pub unsafe fn grow_unique_block(payload: *mut u8, extra_bytes: usize) -> Option<*mut u8> {
    unsafe {
        let base = payload.sub(BLOCK_HEADER_SIZE);
        if *base.cast::<i64>() != 1 {
            return None;
        }
        let used_bytes = (read_bit_length(payload) as usize).div_ceil(BITS_PER_BYTE);
        let needed = BLOCK_HEADER_SIZE + used_bytes + extra_bytes;
        if memory::usable_size(base) >= needed {
            return Some(payload);
        }
        let grown = memory::realloc(base, (needed * 2).max(MIN_GROWN_BLOCK));
        Some(grown.add(BLOCK_HEADER_SIZE))
    }
}

/// Increment the refcount of an rc-managed leaf heap block. `base`
/// points at the block base (the `i64 rc` word). Immortal blocks
/// (statically-allocated rodata payloads, stamped with a negative
/// sentinel rc by codegen) are left untouched. Null is a no-op.
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

/// Decrement the refcount of a closure env block, running its
/// capture-release glue and freeing the block when the count reaches
/// zero. `env` points at the block base (the `i64 rc` word), and the
/// `drop_fn` pointer sits `LENGTH_OFFSET` bytes past it (see the
/// closure env header note on [`LENGTH_OFFSET`]). At zero, the glue,
/// when present (null means no heap-managed capture), is called with
/// the env pointer to release each captured value before the block is
/// freed. Immortal blocks (`rc < 0`) and null are no-ops.
///
/// # Safety
/// `env` must be null or the base of a live closure env block whose
/// `drop_fn` word is either null or a valid `extern "C" fn(*mut u8)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_closure_rc_dec(env: *mut u8) {
    if env.is_null() {
        return;
    }
    unsafe {
        let rc = *env.cast::<i64>();
        if rc < 0 {
            return;
        }
        let remaining = rc - 1;
        if remaining != 0 {
            *env.cast::<i64>() = remaining;
            return;
        }
        let drop_fn = *env.add(LENGTH_OFFSET).cast::<*const u8>();
        if !drop_fn.is_null() {
            let glue = std::mem::transmute::<*const u8, extern "C" fn(*mut u8)>(drop_fn);
            glue(env);
        }
        memory::free(env);
    }
}

/// Deep-copy an rc-managed leaf heap block (`String` / `Binary` /
/// `Bits`), returning a fresh payload pointer with `rc = 1` and the
/// same `bit_length`. Immortal blocks (`rc < 0`) are shared as-is,
/// since rodata payloads are never mutated or freed and a copy would
/// only waste memory. Null returns null.
///
/// The copy always reserves one byte past the payload and writes a
/// NUL there, matching [`alloc_koja_string`]'s layout so string
/// copies keep their C-string borrowability (a harmless extra byte
/// for `Binary` / `Bits`).
///
/// # Safety
/// `payload` must be null or point at the body of a live heap leaf
/// block (the byte right after its `[i64 rc][i64 bit_length]` header).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_heap_deep_copy(payload: *mut u8) -> *mut u8 {
    if payload.is_null() {
        return payload;
    }
    unsafe {
        let base = payload.sub(BLOCK_HEADER_SIZE);
        if *base.cast::<i64>() < 0 {
            return payload;
        }
        let bit_length = read_bit_length(payload);
        let byte_length = (bit_length as usize).div_ceil(BITS_PER_BYTE);
        let copy_base = memory::alloc(BLOCK_HEADER_SIZE + byte_length + 1);
        let copy = write_block_header(copy_base, bit_length);
        ptr::copy_nonoverlapping(payload, copy, byte_length);
        *copy.add(byte_length) = 0;
        copy
    }
}

/// Deep-copy a closure env block through the `copy_fn` glue stamped
/// in its header (see the closure env header note on
/// [`LENGTH_OFFSET`]), returning a fresh env with `rc = 1` and every
/// heap-managed capture recursively copied. Null and immortal envs
/// (`rc < 0`, the static env of a captureless closure) are shared
/// as-is. A heap env whose `copy_fn` is null cannot cross a process
/// boundary. That is a compiler invariant violation, so the runtime
/// aborts rather than alias the env.
///
/// # Safety
/// `env` must be null or the base of a live closure env block whose
/// `copy_fn` word is either null or a valid
/// `extern "C" fn(*mut u8) -> *mut u8`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_closure_deep_copy(env: *mut u8) -> *mut u8 {
    if env.is_null() {
        return env;
    }
    unsafe {
        if *env.cast::<i64>() < 0 {
            return env;
        }
        let copy_fn = *env.add(COPY_FN_OFFSET).cast::<*const u8>();
        assert!(
            !copy_fn.is_null(),
            "koja runtime: closure env crossing a process boundary carries no copy glue \
             (compiler invariant violation)",
        );
        let glue = std::mem::transmute::<*const u8, extern "C" fn(*mut u8) -> *mut u8>(copy_fn);
        glue(env)
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

/// Returns the stable cause code for an `io::Error`. A kind in the table
/// of [`error_kind_code`] has its own code. Any other error is
/// [`ERRNO_BASE`] plus the raw OS errno, which the Koja side shows as
/// `Unknown(errno)`. An error with no errno is [`ERRNO_BASE`] alone.
///
/// `EBADF` has no `ErrorKind` of its own and is the `Closed` cause. The
/// reactor wakes a waiter whose fd another process closed so it
/// observes exactly this errno on its retry.
fn cause_code(e: &io::Error) -> i32 {
    let mapped = error_kind_code(e.kind());
    if mapped != 0 {
        return mapped;
    }
    match e.raw_os_error() {
        Some(EBADF) => ERROR_CODE_CLOSED,
        Some(raw) => ERRNO_BASE + raw,
        None => ERRNO_BASE,
    }
}

/// Cause codes the runtime sets outside the `io::ErrorKind` mapping.
/// Each sits in its alphabetical slot of the table in [`error_kind_code`].
const ERROR_CODE_CLOSED: i32 = 4;
const ERROR_CODE_INVALID_PATH: i32 = 11;
const ERROR_CODE_NAME_NOT_FOUND: i32 = 13;

/// The first cause code past the table. A cause with no code of its own
/// is `ERRNO_BASE` plus the raw OS errno, so the two ranges never meet.
const ERRNO_BASE: i32 = 1000;

/// The failure values for the causes that have no `io::Error`. A path
/// that is not valid UTF-8 cannot name a file, DNS reports a failure
/// outside `errno`, and a bad argument has no errno at all.
pub(crate) const FAILURE_INVALID_PATH: i64 = -(ERROR_CODE_INVALID_PATH as i64);
pub(crate) const FAILURE_NAME_NOT_FOUND: i64 = -(ERROR_CODE_NAME_NOT_FOUND as i64);
pub(crate) const FAILURE_NO_ERRNO: i64 = -(ERRNO_BASE as i64);

/// Maps `io::ErrorKind` to the stable cause codes consumed by the Koja
/// stdlib. The table is alphabetical by cause name, and each Koja enum
/// (`IO.Error` and `File.Error` in `lib/global`, `Socket.Error` in
/// `lib/net`) reads its own subset of it. Keep every side in sync. A 0
/// means the kind has no code, and [`cause_code`] then uses the errno.
///
/// `WouldBlock` has no code because `block_until_ready` parks and
/// retries on `EAGAIN`, so it never reaches Koja code.
pub(crate) fn error_kind_code(kind: io::ErrorKind) -> i32 {
    match kind {
        io::ErrorKind::AddrInUse => 1,
        io::ErrorKind::AlreadyExists => 2,
        io::ErrorKind::BrokenPipe => 3,
        // 4 is `Closed`, from the raw `EBADF` in `cause_code`.
        io::ErrorKind::ConnectionAborted => 5,
        io::ErrorKind::ConnectionRefused => 6,
        io::ErrorKind::ConnectionReset => 7,
        io::ErrorKind::DirectoryNotEmpty => 8,
        io::ErrorKind::HostUnreachable => 9,
        io::ErrorKind::Interrupted => 10,
        // 11 is `InvalidPath`, from a non-UTF-8 path in `cstr_path`.
        io::ErrorKind::IsADirectory => 12,
        // 13 is `NameNotFound`, from `getaddrinfo` in `socket.rs`.
        io::ErrorKind::NetworkUnreachable => 14,
        io::ErrorKind::NotADirectory => 15,
        io::ErrorKind::NotFound => 16,
        io::ErrorKind::PermissionDenied => 17,
        io::ErrorKind::TimedOut => 18,
        _ => 0,
    }
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

/// Returns the negated cause code of `e`. A failed runtime call
/// returns this value, so the cause reaches the caller with the result
/// and no later call has to ask for it. A success value is never
/// negative.
pub fn failure(e: &io::Error) -> i64 {
    -i64::from(cause_code(e))
}

/// Returns the [`failure`] value for the current C `errno`. Used after
/// a foreign library such as BoringSSL fails a syscall outside the
/// runtime's own I/O paths. The caller must make this call before any
/// point where its process can yield, because `errno` belongs to the
/// worker thread.
#[unsafe(no_mangle)]
pub extern "C" fn koja_errno_failure() -> i32 {
    failure(&last_os_error()) as i32
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
/// An argument that is not valid UTF-8 cannot become a `String`, so it
/// ends the program here, before the entry process starts.
///
/// # Safety
/// `argv` must contain at least `argc` valid, NUL-terminated C strings.
/// `out` must point to writable memory large enough for a `KojaList`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_rt_build_argv(argc: i32, argv: *const *const u8, out: *mut KojaList) {
    let skip = 1; // skip argv[0] (program name)
    let total = argc.max(0) as usize;
    let count = total.saturating_sub(skip);
    let buf = memory::alloc(count * std::mem::size_of::<*const u8>()) as *mut *const u8;
    for i in 0..count {
        let c_str =
            unsafe { std::ffi::CStr::from_ptr(*argv.add(i + skip) as *const std::ffi::c_char) };
        let bytes = c_str.to_bytes();
        if std::str::from_utf8(bytes).is_err() {
            exit_before_entry(&format!(
                "command-line argument {} is not valid UTF-8: \"{}\"",
                i + 1,
                String::from_utf8_lossy(bytes),
            ));
        }
        let koja_str = unsafe { alloc_koja_string(bytes) };
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

#[cfg(test)]
mod tests {
    use super::*;

    unsafe fn free_block(payload: *const u8) {
        unsafe { memory::free((payload as *mut u8).sub(BLOCK_HEADER_SIZE)) };
    }

    #[test]
    fn grow_unique_block_keeps_payload_and_reserves_room() {
        let payload = unsafe { alloc_koja_string(b"abc") } as *mut u8;
        let grown = unsafe { grow_unique_block(payload, 1000) }.expect("unique block grows");
        assert_eq!(unsafe { string_payload_bytes(grown) }, b"abc");
        let base = unsafe { grown.sub(BLOCK_HEADER_SIZE) };
        assert!(unsafe { memory::usable_size(base) } >= BLOCK_HEADER_SIZE + 3 + 1000);
        unsafe { free_block(grown) };
    }

    #[test]
    fn grow_unique_block_reuses_slack_without_moving() {
        let payload = unsafe { alloc_koja_string(b"abc") } as *mut u8;
        let grown = unsafe { grow_unique_block(payload, 1000) }.expect("unique block grows");
        let again = unsafe { grow_unique_block(grown, 1) }.expect("unique block grows");
        assert_eq!(again, grown, "growth within slack must not move the block");
        unsafe { free_block(again) };
    }

    #[test]
    fn grow_unique_block_refuses_shared_and_immortal_blocks() {
        let payload = unsafe { alloc_koja_string(b"abc") } as *mut u8;
        let base = unsafe { payload.sub(BLOCK_HEADER_SIZE) };
        unsafe { koja_rc_inc(base) };
        assert!(unsafe { grow_unique_block(payload, 1) }.is_none());
        unsafe { *base.cast::<i64>() = i64::MIN };
        assert!(unsafe { grow_unique_block(payload, 1) }.is_none());
        unsafe { free_block(payload) };
    }
}

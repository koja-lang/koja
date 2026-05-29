//! The single allocator funnel for all Koja heap.
//!
//! Every Koja-managed allocation — codegen-emitted `String` / `Binary`
//! / `Bits` / `List` / `HashTable` / closure / socket buffers, the
//! runtime's own envelope transport and process-local byte runs, and
//! the `CPtr` / `CString` FFI types — routes through this one pair of
//! `alloc` / `free` (plus `realloc`). Codegen calls the `koja_alloc` /
//! `koja_realloc` / `koja_free` symbols; runtime Rust calls the
//! `pub(crate)` helpers. Both bottom out in the same place, so drop
//! glue can free a message's nested heap without ever crossing
//! allocators.
//!
//! **Passthrough invariant.** These are thin wrappers over the libc
//! allocator: a pointer from [`alloc`] is freeable by libc `free` and a
//! libc-`malloc`'d pointer is freeable by [`free`]. That equivalence is
//! load-bearing — `CPtr` / `CString` and user `extern fn malloc/free`
//! hand pointers across the C-ABI boundary in both directions. If a
//! non-libc managed allocator (arena/GC) is ever introduced, the
//! C-interop types must be split back onto literal libc first.
//!
//! Frees are **sizeless** (libc `free` recovers the block size itself),
//! matching codegen's `free(payload - 8)` drop recipe; no `Layout` is
//! threaded to the free site.

use std::process;

/// Allocate `size` bytes, aborting the process on allocation failure so
/// callers never have to null-check. A zero-size request returns
/// whatever the allocator yields (possibly null), matching libc
/// `malloc(0)`.
pub(crate) fn alloc(size: usize) -> *mut u8 {
    let ptr = unsafe { libc::malloc(size) } as *mut u8;
    if ptr.is_null() && size != 0 {
        process::abort();
    }
    ptr
}

/// Resize the block at `ptr` to `size` bytes, aborting on failure.
/// `ptr` may be null (equivalent to [`alloc`]).
///
/// # Safety
/// `ptr` must be null or a live allocation from this funnel.
pub(crate) unsafe fn realloc(ptr: *mut u8, size: usize) -> *mut u8 {
    let new_ptr = unsafe { libc::realloc(ptr.cast(), size) } as *mut u8;
    if new_ptr.is_null() && size != 0 {
        process::abort();
    }
    new_ptr
}

/// Free a block previously returned by [`alloc`] / [`realloc`] (or the
/// codegen `koja_alloc`). Null is a no-op.
///
/// # Safety
/// `ptr` must be null or a live allocation from this funnel that has
/// not already been freed.
pub(crate) unsafe fn free(ptr: *mut u8) {
    unsafe { libc::free(ptr.cast()) };
}

/// Codegen-facing allocation symbol. See [`alloc`].
#[unsafe(no_mangle)]
pub extern "C" fn koja_alloc(size: usize) -> *mut u8 {
    alloc(size)
}

/// Codegen-facing reallocation symbol. See [`realloc`].
///
/// # Safety
/// `ptr` must be null or a live allocation from this funnel.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_realloc(ptr: *mut u8, size: usize) -> *mut u8 {
    unsafe { realloc(ptr, size) }
}

/// Codegen-facing free symbol. See [`free`].
///
/// # Safety
/// `ptr` must be null or a live allocation from this funnel that has
/// not already been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_free(ptr: *mut u8) {
    unsafe { free(ptr) };
}

//! The single allocator funnel for all Koja heap.
//!
//! Every Koja-managed allocation — codegen-emitted `String` / `Binary`
//! / `Bits` / `List` / `HashTable` / closure / socket buffers, the
//! runtime's own envelope transport and process-local byte runs, and
//! the `CPtr` / `CString` FFI types — routes through this one pair of
//! `alloc` / `free` (plus `realloc`). The platform adapter exports the
//! `koja_alloc` / `koja_realloc` / `koja_free` C-ABI symbols over these
//! helpers; both bottom out here, so drop glue can free a message's
//! nested heap without ever crossing allocators.
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
use std::sync::atomic::{AtomicI64, Ordering};

/// Net count of live heap blocks handed out by this funnel: bumped on
/// every non-null [`alloc`] / [`realloc`]-as-alloc and decremented on
/// every [`free`] / [`realloc`]-as-free. It is *not* a byte total — one
/// unit per block — so it returns to its starting value once every
/// allocation made since a checkpoint has been freed.
///
/// Exposed via [`live_blocks`] for steady-state leak fixtures: record
/// the count, run a leak-prone pattern N times, assert the delta is
/// zero. `Relaxed` is sufficient — we only need eventual per-thread
/// visibility and an exact total at a quiesced checkpoint, not ordering
/// against other memory.
static LIVE_BLOCKS: AtomicI64 = AtomicI64::new(0);

/// Allocate `size` bytes, aborting the process on allocation failure so
/// callers never have to null-check. A zero-size request returns
/// whatever the allocator yields (possibly null), matching libc
/// `malloc(0)`.
pub fn alloc(size: usize) -> *mut u8 {
    let ptr = unsafe { libc::malloc(size) } as *mut u8;
    if ptr.is_null() && size != 0 {
        process::abort();
    }
    if !ptr.is_null() {
        LIVE_BLOCKS.fetch_add(1, Ordering::Relaxed);
    }
    ptr
}

/// Resize the block at `ptr` to `size` bytes, aborting on failure.
/// `ptr` may be null (equivalent to [`alloc`]).
///
/// # Safety
/// `ptr` must be null or a live allocation from this funnel.
pub unsafe fn realloc(ptr: *mut u8, size: usize) -> *mut u8 {
    let new_ptr = unsafe { libc::realloc(ptr.cast(), size) } as *mut u8;
    if new_ptr.is_null() && size != 0 {
        process::abort();
    }
    // A resize-in-place / move of a live block leaves the live count
    // unchanged; only the alloc edge (null in, real bytes out) and the
    // free edge (live block in, freed via size 0) move the counter.
    if ptr.is_null() {
        if !new_ptr.is_null() {
            LIVE_BLOCKS.fetch_add(1, Ordering::Relaxed);
        }
    } else if size == 0 {
        LIVE_BLOCKS.fetch_sub(1, Ordering::Relaxed);
    }
    new_ptr
}

/// Free a block previously returned by [`alloc`] / [`realloc`] (or the
/// codegen `koja_alloc`). Null is a no-op.
///
/// # Safety
/// `ptr` must be null or a live allocation from this funnel that has
/// not already been freed.
pub unsafe fn free(ptr: *mut u8) {
    if !ptr.is_null() {
        LIVE_BLOCKS.fetch_sub(1, Ordering::Relaxed);
    }
    unsafe { libc::free(ptr.cast()) };
}

/// Current net count of live heap blocks (see [`LIVE_BLOCKS`]). The
/// adapter's `koja_rt_live_blocks` C-ABI symbol reads this for the
/// steady-state leak fixtures.
pub fn live_blocks() -> i64 {
    LIVE_BLOCKS.load(Ordering::Relaxed)
}

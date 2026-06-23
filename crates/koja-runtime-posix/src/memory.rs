//! C-ABI allocator symbols for the Koja heap.
//!
//! The allocation logic — the libc passthrough and the live-block
//! counter — lives in [`koja_runtime_core::memory`]. These
//! `#[no_mangle]` wrappers export it to codegen (`koja_alloc` /
//! `koja_realloc` / `koja_free`) and to the leak fixtures
//! (`koja_rt_live_blocks`), and keep the symbols rooted in the linked
//! `libkoja_runtime.a` staticlib. See `koja/design/SCHEDULER-PROTOCOL.md`.

pub(crate) use koja_runtime_core::memory::{alloc, free, realloc};

/// Current net count of live heap blocks. The steady-state leak
/// fixtures read this to assert a zero delta across a repeated
/// allocation pattern.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_live_blocks() -> i64 {
    koja_runtime_core::memory::live_blocks()
}

/// Codegen-facing allocation symbol. See [`koja_runtime_core::memory::alloc`].
#[unsafe(no_mangle)]
pub extern "C" fn koja_alloc(size: usize) -> *mut u8 {
    alloc(size)
}

/// Codegen-facing reallocation symbol. See [`koja_runtime_core::memory::realloc`].
///
/// # Safety
/// `ptr` must be null or a live allocation from this funnel.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_realloc(ptr: *mut u8, size: usize) -> *mut u8 {
    unsafe { realloc(ptr, size) }
}

/// Codegen-facing free symbol. See [`koja_runtime_core::memory::free`].
///
/// # Safety
/// `ptr` must be null or a live allocation from this funnel that has
/// not already been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_free(ptr: *mut u8) {
    unsafe { free(ptr) }
}

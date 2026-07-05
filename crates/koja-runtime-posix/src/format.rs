//! `koja_format_*` runtime helpers for `Debug.format` primitive
//! intrinsics. Each helper renders a single primitive value into a
//! freshly allocated length-prefixed Koja string and returns the
//! payload pointer (8 bytes past the `i64 bit_length` header) so
//! callers free with the same `payload - 8` recipe used for every
//! other Koja-emitted heap string.
//!
//! Single source of truth: the LLVM backend's auto-print
//! wrapper (in `koja-runtime-posix/src/intrinsics.rs`) and `Debug.format`
//! intrinsic emitters route through these helpers, so the rendered
//! bytes match the eval interpreter's `Value::Display` impl
//! one-for-one.
//!
//! ## Output shape
//!
//! - Integers: `format!("{}", value)` (decimal digits, optional
//!   leading minus).
//! - Booleans: `"true"` / `"false"`.
//! - Floats: `format!("{:?}", value)`, the round-trippable shape
//!   (`1.0`, not `1`).

use crate::util::alloc_koja_string;

/// Render a 64-bit signed integer as a decimal Koja string.
/// Narrower widths sign- or zero-extend to `i64` at the LLVM call
/// site so this is the single integer ABI. Signedness is the
/// caller's responsibility (see [`koja_format_u64`] for the
/// unsigned escape hatch).
#[unsafe(no_mangle)]
pub extern "C" fn koja_format_i64(value: i64) -> *const u8 {
    let rendered = format!("{value}");
    unsafe { alloc_koja_string(rendered.as_bytes()) }
}

/// Render a 64-bit unsigned integer as a decimal Koja string. Used
/// for `UInt8` / `UInt16` / `UInt32` / `UInt64` debug formatting.
/// The LLVM call site zero-extends the value to `u64` before
/// calling so this is the single unsigned ABI.
#[unsafe(no_mangle)]
pub extern "C" fn koja_format_u64(value: u64) -> *const u8 {
    let rendered = format!("{value}");
    unsafe { alloc_koja_string(rendered.as_bytes()) }
}

/// Render a `Bool` as `"true"` / `"false"`. The LLVM lowering
/// zext's the body's `i1` to `i64` before calling, so any non-zero
/// argument renders `true`.
#[unsafe(no_mangle)]
pub extern "C" fn koja_format_bool(value: i64) -> *const u8 {
    let rendered = if value != 0 { "true" } else { "false" };
    unsafe { alloc_koja_string(rendered.as_bytes()) }
}

/// Render a 32-bit float using Rust's `{:?}` so `1.0` round-trips
/// as `"1.0"` (vs `{}`'s `"1"`). Pairs with `Value::Float32`'s
/// `Display` in `koja-ir-eval` for byte-exact backend
/// symmetry.
#[unsafe(no_mangle)]
pub extern "C" fn koja_format_f32(value: f32) -> *const u8 {
    let rendered = format!("{value:?}");
    unsafe { alloc_koja_string(rendered.as_bytes()) }
}

/// Render a 64-bit float using Rust's `{:?}`, same rule as
/// [`koja_format_f32`].
#[unsafe(no_mangle)]
pub extern "C" fn koja_format_f64(value: f64) -> *const u8 {
    let rendered = format!("{value:?}");
    unsafe { alloc_koja_string(rendered.as_bytes()) }
}

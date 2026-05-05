//! Temporary scaffolding for the alpha LLVM backend's auto-print
//! `main` wrapper. [`expo_alpha_ir_llvm`]'s `Compiler::emit_as_main`
//! ends every emitted `main` with a call to one of the printers below
//! — picked by the body's [`expo_alpha_ir::IRType`] — followed by
//! `ret i64 0`. That gives `expo alpha run --backend=llvm` the same
//! observable behavior (`print value, exit 0`) as the eval
//! interpreter while the language has no user-level prints.
//!
//! Both printers format to match `expo_alpha_ir_eval::Value`'s
//! `Display`: integers as decimal digits, bools as `true` / `false`,
//! trailing `\n`.
//!
//! When `IO.puts` (or equivalent stdlib print primitive) lands:
//!
//! 1. Drop this module.
//! 2. Drop the `mod alpha;` line in [`crate`]'s `lib.rs`.
//! 3. Strip the wrapper from `Compiler::emit_as_main` so the body's
//!    `IRTerminator::Return` flows directly to `main`'s `ret`.

use std::io::{self, Write};

/// Print an `Int`-flavored body value followed by a newline.
/// Narrower widths are sign- or zero-extended to `i64` at the LLVM
/// call site so this is the single integer ABI.
#[unsafe(no_mangle)]
pub extern "C" fn __expo_alpha_print_i64(value: i64) {
    let _ = writeln!(io::stdout(), "{value}");
}

/// Print a `Bool`-flavored body value followed by a newline. The
/// LLVM lowering zext's the body's `i1` to `i64` before calling, so
/// any non-zero argument prints `true`.
#[unsafe(no_mangle)]
pub extern "C" fn __expo_alpha_print_bool(value: i64) {
    let rendered = if value != 0 { "true" } else { "false" };
    let _ = writeln!(io::stdout(), "{rendered}");
}

/// Print a `Float32`-flavored body value followed by a newline.
/// Uses Rust's `{:?}` so `1.0` round-trips as `"1.0"` (vs `{}`'s
/// `"1"`); pairs with `Value::Float32`'s `Display` in
/// `expo-alpha-ir-eval` for byte-exact backend symmetry.
#[unsafe(no_mangle)]
pub extern "C" fn __expo_alpha_print_f32(value: f32) {
    let _ = writeln!(io::stdout(), "{value:?}");
}

/// Print a `Float64`-flavored body value followed by a newline.
/// Same `{:?}` round-trip rule as the f32 printer.
#[unsafe(no_mangle)]
pub extern "C" fn __expo_alpha_print_f64(value: f64) {
    let _ = writeln!(io::stdout(), "{value:?}");
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

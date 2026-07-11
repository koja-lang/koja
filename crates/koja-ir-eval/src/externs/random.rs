//! Externs declared in `lib/global/src/random.koja`.
//!
//! - `@extern "C" fn koja_random_bytes(count: Int64) -> CPtr<UInt8>`
//!   and `@extern "C" fn koja_random_int(min: Int64, max: Int64) -> Int64`:
//!   the runtime entropy primitives `Random.bytes` / `Random.int`
//!   delegate to. Both call straight into [`koja_runtime`] over the
//!   C ABI so eval consumes the same OS entropy the LLVM backend
//!   would.
//!
//! `bytes` returns a length-prefixed Binary payload pointer. Eval
//! wraps it as [`crate::value::Value::CPtr`] before the private
//! runtime-block intrinsic adopts it.

use crate::externs::marshal::pass_through_externs;

pass_through_externs! {
    bytes => fn koja_random_bytes(count: Int64) -> CPtr;
    int => fn koja_random_int(min: Int64, max: Int64) -> Int64;
}

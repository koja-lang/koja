//! Externs declared in `lib/global/src/random.koja`.
//!
//! - `@extern "C" fn koja_random_bytes(count: Int64) -> CPtr<UInt8>`
//!   and `@extern "C" fn koja_random_int(min: Int64, max: Int64) -> Int64`:
//!   the runtime entropy primitives `Random.bytes` / `Random.int`
//!   delegate to. Both call straight into [`koja_runtime`] over the
//!   C ABI so eval consumes the same OS entropy the LLVM backend
//!   would.
//!
//! `bytes` returns a length-prefixed Koja-string payload pointer
//! (the runtime allocates `[i64 bit_length][payload…]` with `malloc`
//! and returns the payload offset). Eval wraps it as
//! [`crate::value::Value::CPtr`]. Consumers walk the standard
//! `CPtr<UInt8>` chain (`.to_string().to_binary()` for the
//! `Random.bytes` body).

use crate::externs::marshal::pass_through_externs;

pass_through_externs! {
    bytes => fn koja_random_bytes(count: Int64) -> CPtr;
    int => fn koja_random_int(min: Int64, max: Int64) -> Int64;
}

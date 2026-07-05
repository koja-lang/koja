//! Externs declared in `lib/global/src/time.koja`.
//!
//! - `@extern "C" fn koja_time_now_millis() -> Int64`: current
//!   wall-clock time in milliseconds since the Unix epoch. Calls
//!   straight into [`koja_runtime`]'s `koja_time_now_millis` over
//!   the C ABI so eval observes the same instant the LLVM backend
//!   would.

use crate::externs::marshal::pass_through_externs;

pass_through_externs! {
    now_millis => fn koja_time_now_millis() -> Int64;
}

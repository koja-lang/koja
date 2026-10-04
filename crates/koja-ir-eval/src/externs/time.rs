//! Externs declared in `lib/global/src/time.koja`.

use crate::externs::marshal::pass_through_externs;

pass_through_externs! {
    now_microseconds => fn koja_time_now_microseconds() -> Int64;
    monotonic_nanoseconds => fn koja_time_monotonic_nanoseconds() -> Int64;
}

//! Externs declared in `lib/global/src/time.koja`.

use crate::externs::marshal::pass_through_externs;

pass_through_externs! {
    now_millis => fn koja_time_now_millis() -> Int64;
}

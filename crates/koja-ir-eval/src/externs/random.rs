//! Externs declared in `lib/global/src/random.koja`.

use crate::externs::marshal::pass_through_externs;

pass_through_externs! {
    bytes => fn koja_random_bytes(count: Int64) -> CPtr;
    int => fn koja_random_int(min: Int64, max: Int64) -> Int64;
}

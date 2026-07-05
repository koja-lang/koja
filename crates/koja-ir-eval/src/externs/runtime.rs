//! Runtime-internal accounting symbols exported by the linked
//! `koja-runtime-posix` staticlib but *not* declared in any stdlib source
//! file: the leak / race oracles the `tests/lang/memory/` and
//! `tests/lang/ownership/` fixtures declare ad-hoc as `@extern "C"` to
//! assert reclaim behavior.
//!
//! - `koja_rt_live_blocks` passes straight through to the native symbol:
//!   the koja-heap allocator (`koja_runtime_core::memory`) is shared, so
//!   both backends read the same live-block counter.
//! - `koja_rt_sched_violations` is read from eval's *own* cooperative core
//!   instead (eval runs its own `ProcessTable`, never the native `SCHED`),
//!   so the kill/park race fixtures genuinely exercise the cooperative
//!   scheduler's transition guard.

use crate::error::RuntimeError;
use crate::externs::marshal::{pass_through_externs, type_mismatch};
use crate::scheduler;
use crate::value::Value;

pass_through_externs! {
    live_blocks => fn koja_rt_live_blocks() -> Int64;
}

pub(super) fn sched_violations(args: &[Value]) -> Result<Value, RuntimeError> {
    let [] = args else {
        return Err(type_mismatch("koja_rt_sched_violations", "()", args));
    };
    Ok(Value::Int(scheduler::sched_violations()))
}

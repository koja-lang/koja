//! Runtime accounting and observability symbols: the leak / race
//! oracles the `tests/lang/memory/` and `tests/lang/ownership/`
//! fixtures declare ad-hoc as `@extern "C"`, plus the observability
//! externs `lib/global/src/runtime.koja` declares behind the `Runtime`
//! API.
//!
//! - `koja_rt_live_blocks` passes straight through to the native symbol:
//!   the koja-heap allocator (`koja_runtime_core::memory`) is shared, so
//!   both backends read the same live-block counter.
//! - Everything else is read from eval's *own* cooperative core (eval
//!   runs its own `ProcessTable`, never the native table), so the
//!   fixtures genuinely observe the cooperative scheduler.

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

pub(super) fn process_count(args: &[Value]) -> Result<Value, RuntimeError> {
    let [] = args else {
        return Err(type_mismatch("koja_rt_process_count", "()", args));
    };
    Ok(Value::Int(scheduler::process_count()))
}

pub(super) fn process_count_by_state(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Int(state)] = args else {
        return Err(type_mismatch(
            "koja_rt_process_count_by_state",
            "(state: Int64)",
            args,
        ));
    };
    Ok(Value::Int(scheduler::process_count_by_state(*state)))
}

pub(super) fn scheduler_count(args: &[Value]) -> Result<Value, RuntimeError> {
    let [] = args else {
        return Err(type_mismatch("koja_rt_scheduler_count", "()", args));
    };
    Ok(Value::Int(scheduler::scheduler_count()))
}

pub(super) fn self_mailbox_depth(args: &[Value]) -> Result<Value, RuntimeError> {
    let [] = args else {
        return Err(type_mismatch("koja_rt_self_mailbox_depth", "()", args));
    };
    Ok(Value::Int(scheduler::self_mailbox_depth()))
}

pub(super) fn mailbox_depth(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Int(pid)] = args else {
        return Err(type_mismatch("koja_rt_mailbox_depth", "(pid: Int64)", args));
    };
    Ok(Value::Int(scheduler::mailbox_depth(*pid)))
}

pub(super) fn process_state(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Int(pid)] = args else {
        return Err(type_mismatch("koja_rt_process_state", "(pid: Int64)", args));
    };
    Ok(Value::Int(scheduler::process_state(*pid)))
}

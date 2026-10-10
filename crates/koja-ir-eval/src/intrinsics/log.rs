//! `LogRuntime.*` family: the runtime plumbing under the stdlib `Log`
//! module. The configuration type is opaque here. It rides the
//! `IRFunction` signature and the handlers move it as a whole
//! [`Value`], the way `TraceRuntime` moves a span record. Mirrors the
//! native `koja_rt_log_*` externs.

use koja_ir::{IRFunction, LogRuntimeMethod};

use super::{IntrinsicCall, helpers};
use crate::error::RuntimeError;
use crate::interpreter::CallResolver;
use crate::scheduler;
use crate::value::Value;

pub(super) fn dispatch<R: CallResolver>(
    method: LogRuntimeMethod,
    call: IntrinsicCall<'_, R>,
) -> Result<Value, RuntimeError> {
    match method {
        LogRuntimeMethod::Config => config(call),
        LogRuntimeMethod::Configure => {
            let config = nth(call.function, call.args, 0, "config")?;
            let level = helpers::arg_int(call.args, 1, "level")?;
            scheduler::log_configure(config, level);
            Ok(Value::Unit)
        }
        LogRuntimeMethod::Enter => Ok(Value::Bool(scheduler::log_enter())),
        LogRuntimeMethod::Leave => {
            scheduler::log_leave();
            Ok(Value::Unit)
        }
        LogRuntimeMethod::Level => Ok(Value::Int(scheduler::log_level())),
    }
}

/// `LogRuntime.config() -> Option<config>`: the stored configuration
/// wrapped in `Option.Some`, or `Option.None` when none is stored.
fn config<R: CallResolver>(call: IntrinsicCall<'_, R>) -> Result<Value, RuntimeError> {
    let option_symbol = helpers::enum_return_symbol(call.function, "LogRuntime.config")?;
    helpers::option_value(option_symbol, call.resolver, scheduler::log_config())
}

/// The argument at `index`, moved out by clone. `Value` clones are
/// reference bumps for the heap-backed variants.
fn nth(
    function: &IRFunction,
    args: &[Value],
    index: usize,
    label: &str,
) -> Result<Value, RuntimeError> {
    args.get(index)
        .cloned()
        .ok_or_else(|| RuntimeError::TypeMismatch {
            detail: format!("{}: missing `{label}` argument", function.symbol),
        })
}

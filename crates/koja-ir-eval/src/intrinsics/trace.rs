//! `TraceRuntime.*` family: the runtime plumbing under the stdlib
//! `Trace` module. The span record type is opaque here. It rides the
//! `IRFunction` signature and the handlers move it as a whole [`Value`],
//! the way `Ref.cast` moves a message. Mirrors the native
//! `koja_rt_trace_*`, `koja_rt_span_*`, and `koja_rt_export_*` externs.

use koja_ir::{IRFunction, TraceRuntimeMethod};
use koja_runtime_core::Context;

use super::{IntrinsicCall, helpers};
use crate::error::RuntimeError;
use crate::interpreter::CallResolver;
use crate::scheduler;
use crate::value::Value;

pub(super) fn dispatch<R: CallResolver>(
    method: TraceRuntimeMethod,
    call: IntrinsicCall<'_, R>,
) -> Result<Value, RuntimeError> {
    match method {
        TraceRuntimeMethod::ExportDropped => Ok(Value::Int(scheduler::export_dropped())),
        TraceRuntimeMethod::ExportPop => export_pop(call),
        TraceRuntimeMethod::ExportPush => {
            scheduler::export_push(nth(call.function, call.args, 0, "record")?);
            Ok(Value::Unit)
        }
        TraceRuntimeMethod::Install => install(call.function, call.args),
        TraceRuntimeMethod::SpanClose => {
            let handle = handle_arg(call.function, call.args)?;
            Ok(scheduler::span_close(handle))
        }
        TraceRuntimeMethod::SpanId => Ok(Value::Int(scheduler::span_id())),
        TraceRuntimeMethod::SpanOpen => {
            let record = nth(call.function, call.args, 0, "record")?;
            Ok(Value::Int(scheduler::span_open(record)))
        }
        TraceRuntimeMethod::SpanPut => {
            let handle = handle_arg(call.function, call.args)?;
            let record = nth(call.function, call.args, 1, "record")?;
            scheduler::span_put(handle, record);
            Ok(Value::Unit)
        }
        TraceRuntimeMethod::SpanTake => {
            let handle = handle_arg(call.function, call.args)?;
            Ok(scheduler::span_take(handle))
        }
    }
}

/// `TraceRuntime.install(context: Process.Context)`: install the
/// four-`Int` struct on the running process.
fn install(function: &IRFunction, args: &[Value]) -> Result<Value, RuntimeError> {
    let Some(Value::Struct { fields, .. }) = args.first() else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "{}: expected a Process.Context struct argument",
                function.symbol
            ),
        });
    };
    let mut words = [0u64; 4];
    for (word, field) in words.iter_mut().zip(fields) {
        let Value::Int(value) = field else {
            return Err(RuntimeError::TypeMismatch {
                detail: format!("{}: Process.Context field is not an Int", function.symbol),
            });
        };
        *word = *value as u64;
    }
    scheduler::set_context(Context::from_words(words));
    Ok(Value::Unit)
}

/// `TraceRuntime.export_pop() -> Option<record>`: the oldest queued
/// record wrapped in `Option.Some`, or `Option.None` when the queue is
/// empty.
fn export_pop<R: CallResolver>(call: IntrinsicCall<'_, R>) -> Result<Value, RuntimeError> {
    let option_symbol = helpers::enum_return_symbol(call.function, "TraceRuntime.export_pop")?;
    helpers::option_value(option_symbol, call.resolver, scheduler::export_pop())
}

/// The `handle: Int` first argument of the `span_*` methods.
fn handle_arg(function: &IRFunction, args: &[Value]) -> Result<i64, RuntimeError> {
    match args.first() {
        Some(Value::Int(handle)) => Ok(*handle),
        other => Err(RuntimeError::TypeMismatch {
            detail: format!("{}: expected an Int handle, got {other:?}", function.symbol),
        }),
    }
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

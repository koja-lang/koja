//! `Ref<M, R>` and `ReplyTo<R>` `@intrinsic` methods, implemented over
//! the cooperative scheduler core (`koja-runtime-core`) that
//! `koja-ir-eval` drives. Each method mirrors the LLVM backend's
//! `koja_rt_*` emitter in `koja-ir-llvm/src/intrinsics/process.rs` (same
//! `(M, Option<ReplyTo<R>>)` envelope shape, same reply-token
//! correlation, same `CallError` mapping), but traffics typed
//! [`Value`]s through the core mailbox instead of serialized bytes.
//!
//! Only [`Ref.call`](RefMethod::Call) suspends: it parks on the caller's
//! one-shot reply slot and yields to the driver until the reply lands or
//! the timeout fires. The rest are non-blocking deliveries that return
//! immediately.

use std::time::Instant;

use koja_ir::{
    IRFunction, IRSymbol, IRType, IRVariantPayload, ProcessMethod, RefMethod, ReplyToMethod,
};
use koja_runtime_core::{MailPark, Pid, Tag, duration_from_user_millis};

use super::{IntrinsicCall, helpers};
use crate::error::RuntimeError;
use crate::interpreter::CallResolver;
use crate::scheduler::{self, EvalMessage, ReplyInfo, YieldOnce};
use crate::value::Value;

pub(super) async fn ref_dispatch<R: CallResolver>(
    method: RefMethod,
    call: IntrinsicCall<'_, R>,
) -> Result<Value, RuntimeError> {
    match method {
        RefMethod::AliveQ => alive(call.function, call.args),
        RefMethod::Call => ref_call(call).await,
        RefMethod::Cast => cast(call.function, call.args),
        RefMethod::Kill => kill(call.function, call.args),
        RefMethod::SelfRef => self_ref(call.function),
        RefMethod::SendAfter => send_after(call.function, call.args),
        RefMethod::Signal => signal(call.function, call.args),
    }
}

pub(super) async fn reply_to_dispatch<R: CallResolver>(
    method: ReplyToMethod,
    call: IntrinsicCall<'_, R>,
) -> Result<Value, RuntimeError> {
    match method {
        ReplyToMethod::Send => reply_send(call),
    }
}

pub(super) fn process_dispatch<R: CallResolver>(
    method: ProcessMethod,
    call: IntrinsicCall<'_, R>,
) -> Result<Value, RuntimeError> {
    match method {
        ProcessMethod::Demonitor => demonitor(call.function, call.args),
        ProcessMethod::Monitor => monitor(call.function, call.args),
        ProcessMethod::Parent => parent(call),
    }
}

// ----- Ref methods --------------------------------------------------------

/// `Ref.self_ref() -> Ref<M, R>`: wrap the running process's PID in the
/// `Ref` struct the return type names (`{ id }`).
fn self_ref(function: &IRFunction) -> Result<Value, RuntimeError> {
    let IRType::Struct(symbol) = &function.return_type else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "`{}` (self_ref) must return a `Ref` struct, got `{:?}`",
                function.symbol, function.return_type,
            ),
        });
    };
    Ok(Value::Struct {
        symbol: symbol.clone(),
        fields: vec![Value::Int(scheduler::current_pid())],
    })
}

/// `Ref.cast(self, msg)`: fire-and-forget, delivering `msg` as a business
/// message with an empty (`None`) reply slot.
fn cast(function: &IRFunction, args: &[Value]) -> Result<Value, RuntimeError> {
    let pid = pid_from_ref(function, args)?;
    let msg = nth(function, args, 1, "message")?;
    scheduler::deliver(pid, business(msg, None));
    Ok(Value::Unit)
}

/// `Ref.send_after(self, msg, delay_ms)`: schedule `msg` as a business
/// message fired after `delay_ms` (clamped non-negative), `None` reply slot.
fn send_after(function: &IRFunction, args: &[Value]) -> Result<Value, RuntimeError> {
    let pid = pid_from_ref(function, args)?;
    let msg = nth(function, args, 1, "message")?;
    let delay_ms = int_arg(function, args, 2, "delay")?;
    let fire_at = Instant::now() + duration_from_user_millis(delay_ms);
    scheduler::schedule_timer(pid, fire_at, business(msg, None));
    Ok(Value::Unit)
}

/// `Ref.signal(self, event)`: deliver a lifecycle signal carrying the
/// event's variant index (Shutdown=0, Interrupt=1, Reload=2), routed to
/// the target's system queue.
fn signal(function: &IRFunction, args: &[Value]) -> Result<Value, RuntimeError> {
    let pid = pid_from_ref(function, args)?;
    let Some(Value::Enum { tag, .. }) = args.get(1) else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "`{}` (signal) expected a `Lifecycle` enum event",
                function.symbol
            ),
        });
    };
    scheduler::deliver(
        pid,
        EvalMessage {
            reply: None,
            tag: Tag::Lifecycle,
            value: Value::Int(i64::from(tag.0)),
        },
    );
    Ok(Value::Unit)
}

/// `Ref.kill(self)`: terminate the target immediately, no signal.
fn kill(function: &IRFunction, args: &[Value]) -> Result<Value, RuntimeError> {
    let pid = pid_from_ref(function, args)?;
    scheduler::kill(pid);
    Ok(Value::Unit)
}

/// `Ref.alive?(self) -> Bool`: whether the target is still running.
fn alive(function: &IRFunction, args: &[Value]) -> Result<Value, RuntimeError> {
    let pid = pid_from_ref(function, args)?;
    Ok(Value::Bool(scheduler::is_alive(pid)))
}

/// `Ref.call(self, msg, timeout) -> Result<R, CallError>`: the
/// synchronous request/reply primitive and the only suspending method.
///
/// Mint a token, deliver `msg` as a business message carrying the caller's
/// `ReplyTo` coordinates, then park on the caller's reply slot and yield.
/// On resume, match the reply token (discarding stale replies from earlier
/// timed-out calls). On deadline, map to `CallError.Timeout` (target alive)
/// or `CallError.ProcessDown` (target gone). Mirrors `emit_call`.
async fn ref_call<R: CallResolver>(call: IntrinsicCall<'_, R>) -> Result<Value, RuntimeError> {
    let target = pid_from_ref(call.function, call.args)?;
    let msg = nth(call.function, call.args, 1, "message")?;
    let timeout_ms = int_arg(call.function, call.args, 2, "timeout")?;
    let result_symbol = helpers::enum_return_symbol(call.function, "Ref.call")?;

    let caller = scheduler::current_pid();
    let token = scheduler::mint_token();
    // Register interest before sending so a fast reply can't beat the caller
    // to the awaited-token check (mirrors native's `koja_rt_call_token`).
    scheduler::set_awaiting_reply(caller, token);
    scheduler::deliver(
        target,
        business(
            msg,
            Some(ReplyInfo {
                caller_pid: caller,
                token,
            }),
        ),
    );
    let deadline = Instant::now() + duration_from_user_millis(timeout_ms);
    let outcome = loop {
        // The timeout check comes before the reply check, so a woken
        // waiter that finds its deadline passed gives up even if a reply
        // squeaked in meanwhile (mirrors native's `koja_rt_call_receive`).
        if Instant::now() >= deadline {
            let variant = if scheduler::is_alive(target) {
                "Timeout"
            } else {
                "ProcessDown"
            };
            break Err(variant);
        }
        // Check the reply slot and park in one hold. The take correlates
        // by token, resolves a dead callee early, and maintains the
        // death-edge waiter index itself, so a callee that dies mid-park
        // wakes this caller instead of leaving it to the timeout.
        match scheduler::take_reply_or_park(caller, token, Some(deadline), target) {
            MailPark::Ready(reply) => break Ok(reply.value),
            MailPark::CalleeDown => break Err("ProcessDown"),
            // A stale leftover from an earlier timed-out call. Drop it
            // and re-check immediately.
            MailPark::Stale(stale) => drop(stale),
            MailPark::Parked | MailPark::Refused => YieldOnce::new().await,
        }
    };
    scheduler::clear_deadline(caller);
    scheduler::clear_awaiting_reply(caller);
    match outcome {
        Ok(value) => helpers::result_value(result_symbol.clone(), call.resolver, Ok(value)),
        Err(variant) => {
            let error = helpers::err_variant_value(&result_symbol, call.resolver, variant)?;
            helpers::result_value(result_symbol.clone(), call.resolver, Err(error))
        }
    }
}

// ----- Process statics ------------------------------------------------------

/// `Process.monitor(target: Pid) -> Process.MonitorRef`: register the
/// running process as a watcher of `target` and wrap the token in the
/// `MonitorRef` struct the return type names.
fn monitor(function: &IRFunction, args: &[Value]) -> Result<Value, RuntimeError> {
    let target = wrapped_int(function, args, "Pid")?;
    let IRType::Struct(symbol) = &function.return_type else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "`{}` (monitor) must return a `MonitorRef` struct, got `{:?}`",
                function.symbol, function.return_type,
            ),
        });
    };
    let token = scheduler::monitor(target);
    Ok(Value::Struct {
        symbol: symbol.clone(),
        fields: vec![Value::Int(token)],
    })
}

/// `Process.demonitor(reference: MonitorRef)`: retract the monitor.
fn demonitor(function: &IRFunction, args: &[Value]) -> Result<Value, RuntimeError> {
    let token = wrapped_int(function, args, "MonitorRef")?;
    scheduler::demonitor(token);
    Ok(Value::Unit)
}

/// `Process.parent() -> Option<Pid>`: the running process's spawner,
/// `None` for the entry process.
fn parent<R: CallResolver>(call: IntrinsicCall<'_, R>) -> Result<Value, RuntimeError> {
    let option_symbol = helpers::enum_return_symbol(call.function, "Process.parent")?;
    let pid = scheduler::parent().map(|pid| Value::Struct {
        symbol: option_some_struct_symbol(&option_symbol, call.resolver),
        fields: vec![Value::Int(pid)],
    });
    helpers::option_value(option_symbol, call.resolver, pid)
}

// ----- ReplyTo methods ----------------------------------------------------

/// `ReplyTo.send(self, reply) -> ReplyTo.Delivery`: route `reply` to the
/// originating caller's one-shot reply slot, stamped with `self`'s correlation
/// token. Returns `Delivery.Delivered` if the caller was still awaiting the
/// reply, `Delivery.Expired` if it had moved on. Mirrors `emit_reply_send`.
fn reply_send<R: CallResolver>(call: IntrinsicCall<'_, R>) -> Result<Value, RuntimeError> {
    let coords = reply_to_coords(call.function, call.args)?;
    let reply = nth(call.function, call.args, 1, "reply")?;
    let delivery_symbol = helpers::enum_return_symbol(call.function, "ReplyTo.send")?;
    let variant = if scheduler::reply(coords, reply) {
        "Delivered"
    } else {
        "Expired"
    };
    helpers::unit_variant_value(&delivery_symbol, call.resolver, variant)
}

// ----- message materialization --------------------------------------------

/// Build the `(M, Option<ReplyTo<R>>)` value a delivered business
/// message binds into a receive arm's payload local. The receiver's arm
/// `payload_type` names the tuple, its second element names the
/// `Option<ReplyTo<R>>`, and (for a call) the `Some` variant names the
/// `ReplyTo` struct, so the whole shape is recovered from the decls,
/// mirroring the LLVM receive-side typed load. A `None` reply slot is a
/// cast / timer fire. `Some` carries the caller's `ReplyTo` coordinates.
pub(crate) fn build_business_payload<R: CallResolver>(
    envelope_type: &IRType,
    message: EvalMessage,
    resolver: &R,
) -> Value {
    let IRType::Tuple(elements) = envelope_type else {
        panic!(
            "interpreter: business receive arm payload `{envelope_type:?}` is not a tuple \
             (seal invariant violation)"
        );
    };
    let [_message_type, IRType::Enum(option_symbol)] = elements.as_slice() else {
        panic!(
            "interpreter: business envelope `{envelope_type:?}` does not end in an `Option` enum \
             (seal invariant violation)"
        );
    };
    let reply_to = message.reply.map(|info| Value::Struct {
        symbol: option_some_struct_symbol(option_symbol, resolver),
        fields: vec![Value::Int(info.caller_pid), Value::Int(info.token)],
    });
    let option =
        helpers::option_value(option_symbol.clone(), resolver, reply_to).unwrap_or_else(|error| {
            panic!("interpreter: business envelope `Option` (seal invariant violation): {error:?}")
        });
    Value::Tuple(vec![message.value, option])
}

/// Recover the payload struct symbol from an `Option<T>` enum decl's
/// `Some` variant (`T` = `ReplyTo<R>` for call replies, `Pid` for
/// `Process.parent`).
fn option_some_struct_symbol<R: CallResolver>(option_symbol: &IRSymbol, resolver: &R) -> IRSymbol {
    let option_decl = resolver.enum_decl(option_symbol.mangled()).unwrap_or_else(|| {
        panic!("interpreter: `Option` enum `{option_symbol}` missing from IR (seal invariant violation)")
    });
    let some = option_decl
        .variants
        .iter()
        .find(|variant| variant.name == "Some")
        .unwrap_or_else(|| {
            panic!("interpreter: `Option` `{option_symbol}` has no `Some` variant (seal invariant violation)")
        });
    match &some.payload {
        IRVariantPayload::Tuple(types) => match types.as_slice() {
            [IRType::Struct(symbol)] => symbol.clone(),
            other => panic!(
                "interpreter: `Option.Some` payload `{other:?}` is not a single struct \
                 (seal invariant violation)"
            ),
        },
        other => panic!(
            "interpreter: `Option.Some` payload `{other:?}` is not a tuple (seal invariant violation)"
        ),
    }
}

// ----- shared helpers -----------------------------------------------------

/// A business-tagged [`EvalMessage`] with the given reply coordinates.
fn business(value: Value, reply: Option<ReplyInfo>) -> EvalMessage {
    EvalMessage {
        reply,
        tag: Tag::Business,
        value,
    }
}

/// Read the PID out of a `Ref<M, R>` self value (`{ id }`, field 0).
fn pid_from_ref(function: &IRFunction, args: &[Value]) -> Result<Pid, RuntimeError> {
    wrapped_int(function, args, "Ref")
}

/// Read the single integer field out of arg 0's struct wrapper
/// (`Ref` / `Pid` / `MonitorRef` all lay out as `{ i64 }`).
fn wrapped_int(function: &IRFunction, args: &[Value], kind: &str) -> Result<i64, RuntimeError> {
    match args.first() {
        Some(Value::Struct { fields, .. }) => match fields.first() {
            Some(Value::Int(value)) => Ok(*value),
            _ => Err(self_shape_error(function, kind)),
        },
        _ => Err(self_shape_error(function, kind)),
    }
}

/// Read the `(caller_pid, token)` out of a `ReplyTo<R>` self value
/// (`{ id, token }`, fields 0 and 1).
fn reply_to_coords(function: &IRFunction, args: &[Value]) -> Result<ReplyInfo, RuntimeError> {
    let Some(Value::Struct { fields, .. }) = args.first() else {
        return Err(self_shape_error(function, "ReplyTo"));
    };
    match (fields.first(), fields.get(1)) {
        (Some(Value::Int(id)), Some(Value::Int(token))) => Ok(ReplyInfo {
            caller_pid: *id,
            token: *token,
        }),
        _ => Err(self_shape_error(function, "ReplyTo")),
    }
}

fn self_shape_error(function: &IRFunction, kind: &str) -> RuntimeError {
    RuntimeError::TypeMismatch {
        detail: format!(
            "`{}` expected a `{kind}` self value with integer field(s)",
            function.symbol,
        ),
    }
}

/// Clone the `index`-th argument, erroring when it is absent.
fn nth(
    function: &IRFunction,
    args: &[Value],
    index: usize,
    what: &str,
) -> Result<Value, RuntimeError> {
    args.get(index)
        .cloned()
        .ok_or_else(|| RuntimeError::TypeMismatch {
            detail: format!("`{}` missing {what} (param #{index})", function.symbol),
        })
}

/// Read the `index`-th argument as an `Int`.
fn int_arg(
    function: &IRFunction,
    args: &[Value],
    index: usize,
    what: &str,
) -> Result<i64, RuntimeError> {
    match args.get(index) {
        Some(Value::Int(value)) => Ok(*value),
        _ => Err(RuntimeError::TypeMismatch {
            detail: format!(
                "`{}` expected an integer {what} (param #{index})",
                function.symbol
            ),
        }),
    }
}

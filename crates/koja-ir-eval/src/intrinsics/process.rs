//! `Ref<M, R>` and `ReplyTo<R>` `@intrinsic` methods, implemented over
//! the cooperative scheduler core (`koja-runtime-core`) that
//! `koja-ir-eval` drives. Each method mirrors the LLVM backend's
//! `koja_rt_*` emitter in `koja-ir-llvm/src/intrinsics/process.rs` (same
//! `Pair<M, Option<ReplyTo<R>>>` envelope shape, same reply-token
//! correlation, same `CallError` mapping), but traffics typed
//! [`Value`]s through the core mailbox instead of serialized bytes.
//!
//! Only [`Ref.call`](RefMethod::Call) suspends: it parks on the caller's
//! one-shot reply slot and yields to the driver until the reply lands or
//! the timeout fires. The rest are non-blocking deliveries that return
//! immediately.

use std::time::{Duration, Instant};

use koja_ir::{
    IRFunction, IRSymbol, IRType, IRVariantPayload, IRVariantTag, RefMethod, ReplyToMethod,
};
use koja_runtime_core::{Pid, Tag};

use super::helpers;
use crate::error::RuntimeError;
use crate::interpreter::CallResolver;
use crate::scheduler::{self, EvalMessage, ReplyInfo, YieldOnce};
use crate::value::Value;

/// `Option<T>::Some` tag (declaration order, v1 convention shared with
/// [`helpers`]). Used to recover the `ReplyTo` payload type from an
/// `Option<ReplyTo<R>>` decl when materializing a delivered call.
const SOME_TAG: IRVariantTag = IRVariantTag(0);

pub(super) async fn ref_dispatch<R: CallResolver>(
    method: RefMethod,
    function: &IRFunction,
    args: &[Value],
    resolver: &R,
) -> Result<Value, RuntimeError> {
    match method {
        RefMethod::AliveQ => alive(function, args),
        RefMethod::Call => call(function, args, resolver).await,
        RefMethod::Cast => cast(function, args),
        RefMethod::Kill => kill(function, args),
        RefMethod::SelfRef => self_ref(function),
        RefMethod::SendAfter => send_after(function, args),
        RefMethod::Signal => signal(function, args),
    }
}

pub(super) async fn reply_to_dispatch<R: CallResolver>(
    method: ReplyToMethod,
    function: &IRFunction,
    args: &[Value],
    resolver: &R,
) -> Result<Value, RuntimeError> {
    match method {
        ReplyToMethod::Send => reply_send(function, args, resolver),
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
    let fire_at = Instant::now() + Duration::from_millis(delay_ms.max(0) as u64);
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
async fn call<R: CallResolver>(
    function: &IRFunction,
    args: &[Value],
    resolver: &R,
) -> Result<Value, RuntimeError> {
    let target = pid_from_ref(function, args)?;
    let msg = nth(function, args, 1, "message")?;
    let timeout_ms = int_arg(function, args, 2, "timeout")?;
    let result_symbol = helpers::enum_return_symbol(function, "Ref.call")?;

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

    let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(0) as u64);
    loop {
        if let Some(reply) = scheduler::take_reply(caller) {
            // Correlate by token. A mismatch is a stale reply from an
            // earlier call that already timed out, so discard and keep waiting.
            if reply.reply.map(|info| info.token) == Some(token) {
                scheduler::clear_awaiting_reply(caller);
                return Ok(helpers::result_value(
                    result_symbol.clone(),
                    Ok(reply.value),
                ));
            }
            continue;
        }
        if Instant::now() >= deadline {
            scheduler::clear_awaiting_reply(caller);
            let variant = if scheduler::is_alive(target) {
                "Timeout"
            } else {
                "ProcessDown"
            };
            let error = helpers::err_variant_value(&result_symbol, resolver, variant)?;
            return Ok(helpers::result_value(result_symbol.clone(), Err(error)));
        }
        scheduler::park_reply(caller, Some(deadline));
        YieldOnce::new().await;
    }
}

// ----- ReplyTo methods ----------------------------------------------------

/// `ReplyTo.send(self, reply) -> ReplyTo.Delivery`: route `reply` to the
/// originating caller's one-shot reply slot, stamped with `self`'s correlation
/// token. Returns `Delivery.Delivered` if the caller was still awaiting the
/// reply, `Delivery.Expired` if it had moved on. Mirrors `emit_reply_send`.
fn reply_send<R: CallResolver>(
    function: &IRFunction,
    args: &[Value],
    resolver: &R,
) -> Result<Value, RuntimeError> {
    let coords = reply_to_coords(function, args)?;
    let reply = nth(function, args, 1, "reply")?;
    let delivery_symbol = helpers::enum_return_symbol(function, "ReplyTo.send")?;
    let variant = if scheduler::reply(coords, reply) {
        "Delivered"
    } else {
        "Expired"
    };
    helpers::unit_variant_value(&delivery_symbol, resolver, variant)
}

// ----- message materialization --------------------------------------------

/// Build the `Pair<M, Option<ReplyTo<R>>>` value a delivered business
/// message binds into a receive arm's payload local. The receiver's arm
/// `payload_type` names the `Pair`, its second field names the
/// `Option<ReplyTo<R>>`, and (for a call) the `Some` variant names the
/// `ReplyTo` struct, so the whole shape is recovered from the decls,
/// mirroring the LLVM receive-side typed load. A `None` reply slot is a
/// cast / timer fire. `Some` carries the caller's `ReplyTo` coordinates.
pub(crate) fn build_business_payload<R: CallResolver>(
    pair_type: &IRType,
    message: EvalMessage,
    resolver: &R,
) -> Value {
    let IRType::Struct(pair_symbol) = pair_type else {
        panic!(
            "interpreter: business receive arm payload `{pair_type:?}` is not a `Pair` struct \
             (seal invariant violation)"
        );
    };
    let pair_decl = resolver.struct_decl(pair_symbol.mangled()).unwrap_or_else(|| {
        panic!("interpreter: `Pair` struct `{pair_symbol}` missing from IR (seal invariant violation)")
    });
    let IRType::Enum(option_symbol) = reply_field_type(pair_decl, pair_symbol) else {
        panic!(
            "interpreter: `Pair` `{pair_symbol}` second field is not an `Option` enum \
             (seal invariant violation)"
        );
    };
    let reply_to = message.reply.map(|info| Value::Struct {
        symbol: reply_to_symbol(option_symbol, resolver),
        fields: vec![Value::Int(info.caller_pid), Value::Int(info.token)],
    });
    Value::Struct {
        symbol: pair_symbol.clone(),
        fields: vec![
            message.value,
            helpers::option_value(option_symbol.clone(), reply_to),
        ],
    }
}

/// The `Pair`'s second (reply) field type, the `Option<ReplyTo<R>>`.
fn reply_field_type<'a>(
    pair_decl: &'a koja_ir::IRStructDecl,
    pair_symbol: &IRSymbol,
) -> &'a IRType {
    pair_decl
        .fields
        .get(1)
        .map(|field| &field.ir_type)
        .unwrap_or_else(|| {
            panic!(
                "interpreter: `Pair` struct `{pair_symbol}` has no second (reply) field \
                 (seal invariant violation)"
            )
        })
}

/// Recover the `ReplyTo<R>` struct symbol from an `Option<ReplyTo<R>>`
/// enum decl's `Some` payload.
fn reply_to_symbol<R: CallResolver>(option_symbol: &IRSymbol, resolver: &R) -> IRSymbol {
    let option_decl = resolver.enum_decl(option_symbol.mangled()).unwrap_or_else(|| {
        panic!("interpreter: `Option` enum `{option_symbol}` missing from IR (seal invariant violation)")
    });
    let some = option_decl
        .variants
        .iter()
        .find(|variant| variant.tag == SOME_TAG)
        .unwrap_or_else(|| {
            panic!("interpreter: `Option` `{option_symbol}` has no `Some` variant (seal invariant violation)")
        });
    match &some.payload {
        IRVariantPayload::Tuple(types) => match types.as_slice() {
            [IRType::Struct(symbol)] => symbol.clone(),
            other => panic!(
                "interpreter: `Option.Some` payload `{other:?}` is not a single `ReplyTo` struct \
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
    match args.first() {
        Some(Value::Struct { fields, .. }) => match fields.first() {
            Some(Value::Int(pid)) => Ok(*pid),
            _ => Err(self_shape_error(function, "Ref")),
        },
        _ => Err(self_shape_error(function, "Ref")),
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

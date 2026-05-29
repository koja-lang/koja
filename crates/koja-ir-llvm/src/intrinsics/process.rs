//! `Ref<M, R>` and `ReplyTo<R>` `@intrinsic` emitters. Single call
//! site for the per-method `koja_rt_*` declares minted in
//! [`crate::runtime`]; the matching runtime symbols live in
//! `koja-runtime/src/scheduler.rs`.
//!
//! Per-method dispatch:
//!
//! - [`emit_ref`] dispatches each [`RefMethod`] to its emitter:
//!   - `SelfRef` → `koja_rt_self()` wrapped in the `Ref<M, R>`
//!     struct shape.
//!   - `Cast` → wrap `msg` in a `Pair<M, Option<ReplyTo<R>>>`
//!     envelope with the second field set to `Option::None`,
//!     then `koja_rt_send(pid, blob, sizeof(envelope))`. The
//!     receive-side `pair: Pair<M, Option<ReplyTo<R>>> ->` arm
//!     in the `Process.run` default body reads the same shape.
//!   - `Signal` → `koja_rt_send_lifecycle(pid, variant)` reading
//!     the `Lifecycle` enum tag byte.
//!   - `Kill` → `koja_rt_kill(pid)`.
//!   - `AliveQ` → `koja_rt_is_process_alive(pid) != 0` truncated
//!     back down to `i1`.
//!   - `SendAfter` → wrap `msg` in the same `Pair` envelope as
//!     `Cast` (`Option::None` reply slot), then call
//!     `koja_rt_send_after(pid, blob, sizeof(envelope), delay_ms)`.
//!   - `Call` → wrap `msg` in `Pair<M, Option::Some(ReplyTo {
//!     id: caller_pid })>`, call `koja_rt_send`, then
//!     `koja_rt_receive_timeout(timeout)`. Three-way dispatch
//!     on the result: non-null envelope → deserialize `R` →
//!     `Result.Ok(R)`; null envelope + target alive → `Result.Err(
//!     CallError.Timeout)`; null envelope + target dead →
//!     `Result.Err(CallError.ProcessDown)`.
//! - [`emit_reply_to`] dispatches the single [`ReplyToMethod::Send`]
//!   to a serializer + `koja_rt_send`. The reply payload is the
//!   bare `R` value (no `Pair` envelope) — the call-side
//!   `koja_rt_receive_timeout` reader pulls `R` straight off the
//!   envelope's `+8` payload offset.

use inkwell::IntPredicate;
use inkwell::types::{BasicType, BasicTypeEnum, StructType};
use inkwell::values::{ArrayValue, BasicValueEnum, FunctionValue, IntValue, PointerValue};
use koja_ir::mangling::global_primitive_symbol;
use koja_ir::{
    IRFunction, IRSymbol, IRType, IRVariantPayload, IRVariantTag, RefMethod, ReplyToMethod,
};

use crate::ctx::EmitContext;
use crate::emit::enums::build_enum_value;
use crate::emit::inkwell_err;
use crate::emit::process::serialize_to_stack;
use crate::error::LlvmError;
use crate::runtime::{
    declare_rt_is_process_alive_extern, declare_rt_kill_extern, declare_rt_receive_timeout_extern,
    declare_rt_self_extern, declare_rt_send_after_extern, declare_rt_send_extern,
    declare_rt_send_lifecycle_extern,
};
use crate::types::{ir_basic_type, value_basic_type};

pub(super) fn emit_ref<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    method: RefMethod,
) -> Result<(), LlvmError> {
    match method {
        RefMethod::AliveQ => emit_alive(ctx, function, llvm_function),
        RefMethod::Call => emit_call(ctx, function, llvm_function),
        RefMethod::Cast => emit_cast(ctx, function, llvm_function),
        RefMethod::Kill => emit_kill(ctx, function, llvm_function),
        RefMethod::SelfRef => emit_self_ref(ctx, function, llvm_function),
        RefMethod::SendAfter => emit_send_after(ctx, function, llvm_function),
        RefMethod::Signal => emit_signal(ctx, function, llvm_function),
    }
}

pub(super) fn emit_reply_to<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    method: ReplyToMethod,
) -> Result<(), LlvmError> {
    match method {
        ReplyToMethod::Send => emit_reply_send(ctx, function, llvm_function),
    }
}

// ----- Ref method emitters -------------------------------------------------

/// `Ref.self_ref() -> Ref<M, R>` — call `koja_rt_self()` and wrap
/// the returned pid in the `Ref` struct value the function's
/// return type already specifies.
fn emit_self_ref<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let entry_bb = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry_bb);

    let self_fn = declare_rt_self_extern(ctx);
    let pid = ctx
        .builder
        .build_call(self_fn, &[], "current_pid")
        .map_err(|e| inkwell_err("build_call koja_rt_self", e))?
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| LlvmError::Codegen("koja_rt_self did not produce a value".to_string()))?
        .into_int_value();

    let ref_struct = match &function.return_type {
        IRType::Struct(symbol) => ctx.layouts.struct_type(symbol.mangled()),
        other => {
            return Err(LlvmError::Codegen(format!(
                "LLVM emit: `Ref.self_ref` returns `{other:?}` (expected Struct) — \
                 IR seal invariant violation",
            )));
        }
    };
    let mut ref_value = ref_struct.get_undef();
    ref_value = ctx
        .builder
        .build_insert_value(ref_value, pid, 0, "ref_pid")
        .map_err(|e| inkwell_err("build_insert_value ref_pid", e))?
        .into_struct_value();
    ctx.builder
        .build_return(Some(&ref_value))
        .map(|_| ())
        .map_err(|e| inkwell_err("build_return self_ref", e))
}

/// `Ref.cast(self, msg: M)` — wrap `msg` in a `Pair<M, Option<
/// ReplyTo<R>>>` envelope with the second field set to `Option::
/// None`, then route through `koja_rt_send`. The receive-side
/// `pair: Pair<M, Option<ReplyTo<R>>> ->` arm in the `Process.run`
/// default body reads the same shape.
fn emit_cast<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let entry_bb = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry_bb);

    let pid = pid_from_self(ctx, llvm_function, function)?;
    let (msg_value, msg_ir_type) = nth_param(function, llvm_function, 1)?;
    let msg_llvm = value_basic_type(ctx, msg_ir_type)?;
    let none_payload = option_none_payload(ctx);
    let (envelope_ptr, envelope_size) =
        build_pair_envelope_alloca(ctx, "cast_envelope", msg_llvm, msg_value, none_payload)?;

    let send_fn = declare_rt_send_extern(ctx);
    ctx.builder
        .build_call(
            send_fn,
            &[pid.into(), envelope_ptr.into(), envelope_size.into()],
            "",
        )
        .map_err(|e| inkwell_err("build_call koja_rt_send (cast)", e))?;
    ctx.builder
        .build_return(None)
        .map(|_| ())
        .map_err(|e| inkwell_err("build_return cast", e))
}

/// `Ref.send_after(self, msg: M, delay_ms: Int)` — same `Pair<M,
/// Option<ReplyTo<R>>>` envelope as [`emit_cast`] (`Option::None`
/// reply slot), routed through `koja_rt_send_after` with the
/// trailing delay parameter.
fn emit_send_after<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let entry_bb = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry_bb);

    let pid = pid_from_self(ctx, llvm_function, function)?;
    let (msg_value, msg_ir_type) = nth_param(function, llvm_function, 1)?;
    let (delay_value, _) = nth_param(function, llvm_function, 2)?;
    let msg_llvm = value_basic_type(ctx, msg_ir_type)?;
    let none_payload = option_none_payload(ctx);
    let (envelope_ptr, envelope_size) = build_pair_envelope_alloca(
        ctx,
        "send_after_envelope",
        msg_llvm,
        msg_value,
        none_payload,
    )?;

    let delay = delay_value.into_int_value();
    let send_after_fn = declare_rt_send_after_extern(ctx);
    ctx.builder
        .build_call(
            send_after_fn,
            &[
                pid.into(),
                envelope_ptr.into(),
                envelope_size.into(),
                delay.into(),
            ],
            "",
        )
        .map_err(|e| inkwell_err("build_call koja_rt_send_after", e))?;
    ctx.builder
        .build_return(None)
        .map(|_| ())
        .map_err(|e| inkwell_err("build_return send_after", e))
}

/// `Ref.call(self, msg: M, timeout: Int) -> Result<R, CallError>`
/// — the synchronous request/reply primitive.
///
/// Build a `Pair<M, Option<ReplyTo<R>>>` envelope with the second
/// field set to `Option::Some(ReplyTo { id: caller_pid })` (the
/// caller's pid serves as the one-shot reply mailbox), `koja_rt_
/// send` it to the target, then `koja_rt_receive_timeout(timeout)`
/// against the caller's mailbox. Three-way dispatch on the result:
///
/// - non-null envelope → load `R` from `envelope + 8` →
///   `Result.Ok(R)`.
/// - null envelope + target alive → `Result.Err(CallError.Timeout)`.
/// - null envelope + target dead → `Result.Err(CallError.ProcessDown)`.
fn emit_call<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let entry_bb = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry_bb);

    let target_pid = pid_from_self(ctx, llvm_function, function)?;
    let (msg_value, msg_ir_type) = nth_param(function, llvm_function, 1)?;
    let (timeout_value, _) = nth_param(function, llvm_function, 2)?;
    let timeout = timeout_value.into_int_value();
    let msg_llvm = value_basic_type(ctx, msg_ir_type)?;

    let result_symbol = match &function.return_type {
        IRType::Enum(symbol) => symbol.clone(),
        other => {
            return Err(LlvmError::Codegen(format!(
                "LLVM emit: `Ref.call` returns `{other:?}` (expected Enum) — \
                 IR seal invariant violation",
            )));
        }
    };
    let reply_ir_type = ok_payload_field_type(ctx, &result_symbol)?;
    let reply_llvm = value_basic_type(ctx, &reply_ir_type)?;
    let call_error_symbol = global_primitive_symbol("CallError");

    let self_fn = declare_rt_self_extern(ctx);
    let caller_pid = ctx
        .builder
        .build_call(self_fn, &[], "caller_pid")
        .map_err(|e| inkwell_err("build_call koja_rt_self (call)", e))?
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| LlvmError::Codegen("koja_rt_self did not produce a value".to_string()))?
        .into_int_value();
    let some_payload = option_some_payload(ctx, caller_pid)?;
    let (envelope_ptr, envelope_size) =
        build_pair_envelope_alloca(ctx, "call_envelope", msg_llvm, msg_value, some_payload)?;
    let send_fn = declare_rt_send_extern(ctx);
    ctx.builder
        .build_call(
            send_fn,
            &[target_pid.into(), envelope_ptr.into(), envelope_size.into()],
            "",
        )
        .map_err(|e| inkwell_err("build_call koja_rt_send (call)", e))?;

    let reply_slot = ctx.build_entry_alloca(reply_llvm, "reply_payload");
    let reply_cap = ctx
        .context
        .i64_type()
        .const_int(ctx.layouts.target_data.get_abi_size(&reply_llvm), false);
    let receive_fn = declare_rt_receive_timeout_extern(ctx);
    let reply_tag = ctx
        .builder
        .build_call(
            receive_fn,
            &[reply_slot.into(), reply_cap.into(), timeout.into()],
            "reply_tag",
        )
        .map_err(|e| inkwell_err("build_call koja_rt_receive_timeout (call)", e))?
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| {
            LlvmError::Codegen("koja_rt_receive_timeout did not produce a value".to_string())
        })?
        .into_int_value();

    let timeout_check_bb = ctx
        .context
        .append_basic_block(llvm_function, "call_timeout_check");
    let got_reply_bb = ctx
        .context
        .append_basic_block(llvm_function, "call_got_reply");
    let build_timeout_bb = ctx
        .context
        .append_basic_block(llvm_function, "call_build_timeout");
    let build_down_bb = ctx
        .context
        .append_basic_block(llvm_function, "call_build_down");
    let merge_bb = ctx.context.append_basic_block(llvm_function, "call_merge");

    let none_tag = ctx.context.i64_type().const_int(-1i64 as u64, true);
    let is_none = ctx
        .builder
        .build_int_compare(IntPredicate::EQ, reply_tag, none_tag, "reply_is_none")
        .map_err(|e| inkwell_err("build_int_compare reply_is_none", e))?;
    ctx.builder
        .build_conditional_branch(is_none, timeout_check_bb, got_reply_bb)
        .map_err(|e| inkwell_err("build_conditional_branch reply_is_none", e))?;

    ctx.builder.position_at_end(timeout_check_bb);
    let alive_fn = declare_rt_is_process_alive_extern(ctx);
    let alive_i64 = ctx
        .builder
        .build_call(alive_fn, &[target_pid.into()], "target_alive_i64")
        .map_err(|e| inkwell_err("build_call koja_rt_is_process_alive (call)", e))?
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| {
            LlvmError::Codegen("koja_rt_is_process_alive did not produce a value".to_string())
        })?
        .into_int_value();
    let zero_i64 = ctx.context.i64_type().const_int(0, false);
    let target_alive = ctx
        .builder
        .build_int_compare(IntPredicate::NE, alive_i64, zero_i64, "target_alive")
        .map_err(|e| inkwell_err("build_int_compare target_alive", e))?;
    ctx.builder
        .build_conditional_branch(target_alive, build_timeout_bb, build_down_bb)
        .map_err(|e| inkwell_err("build_conditional_branch target_alive", e))?;

    ctx.builder.position_at_end(build_timeout_bb);
    let timeout_result = build_call_error_result(
        ctx,
        &result_symbol,
        &call_error_symbol,
        CALL_ERROR_TIMEOUT_TAG,
    )?;
    ctx.builder
        .build_unconditional_branch(merge_bb)
        .map_err(|e| inkwell_err("build_unconditional_branch call_timeout merge", e))?;
    let timeout_block = ctx.builder.get_insert_block().expect(
        "EmitContext::emit_call lost the build_timeout insertion block before the merge phi",
    );

    ctx.builder.position_at_end(build_down_bb);
    let down_result = build_call_error_result(
        ctx,
        &result_symbol,
        &call_error_symbol,
        CALL_ERROR_PROCESS_DOWN_TAG,
    )?;
    ctx.builder
        .build_unconditional_branch(merge_bb)
        .map_err(|e| inkwell_err("build_unconditional_branch call_down merge", e))?;
    let down_block = ctx
        .builder
        .get_insert_block()
        .expect("EmitContext::emit_call lost the build_down insertion block before the merge phi");

    ctx.builder.position_at_end(got_reply_bb);
    let reply_value = ctx
        .builder
        .build_load(reply_llvm, reply_slot, "reply_value")
        .map_err(|e| inkwell_err("build_load reply_value", e))?;
    let ok_result = build_enum_value(
        ctx,
        &result_symbol,
        IRVariantTag(RESULT_OK_TAG),
        &[reply_value],
    )?;
    ctx.builder
        .build_unconditional_branch(merge_bb)
        .map_err(|e| inkwell_err("build_unconditional_branch call_ok merge", e))?;
    let ok_block = ctx
        .builder
        .get_insert_block()
        .expect("EmitContext::emit_call lost the got_reply insertion block before the merge phi");

    ctx.builder.position_at_end(merge_bb);
    let result_outer = ctx.enum_outer_type(result_symbol.mangled());
    let result_phi = ctx
        .builder
        .build_phi(result_outer, "call_result")
        .map_err(|e| inkwell_err("build_phi call_result", e))?;
    result_phi.add_incoming(&[
        (&timeout_result, timeout_block),
        (&down_result, down_block),
        (&ok_result, ok_block),
    ]);
    ctx.builder
        .build_return(Some(&result_phi.as_basic_value()))
        .map(|_| ())
        .map_err(|e| inkwell_err("build_return call result", e))
}

/// `Ref.signal(self, event: Lifecycle)` — pull the lifecycle
/// variant byte (offset 0 of the enum's outer struct) and call
/// `koja_rt_send_lifecycle(pid, variant)`. The runtime maps
/// variant indices `0=Shutdown, 1=Interrupt, 2=Reload`, matching
/// the AST declaration order in `Global.process`.
fn emit_signal<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let entry_bb = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry_bb);

    let pid = pid_from_self(ctx, llvm_function, function)?;
    let (event_value, event_ir_type) = nth_param(function, llvm_function, 1)?;
    let event_llvm = ir_basic_type(ctx, event_ir_type)?;
    let event_alloca = ctx.build_entry_alloca(event_llvm, "event_buf");
    ctx.builder
        .build_store(event_alloca, event_value)
        .map_err(|e| inkwell_err("build_store signal event", e))?;
    let i8_ty = ctx.context.i8_type();
    let variant_byte = ctx
        .builder
        .build_load(i8_ty, event_alloca, "variant_byte")
        .map_err(|e| inkwell_err("build_load variant_byte", e))?
        .into_int_value();
    let variant_i64 = ctx
        .builder
        .build_int_z_extend(variant_byte, ctx.context.i64_type(), "variant_i64")
        .map_err(|e| inkwell_err("build_int_z_extend variant", e))?;

    let signal_fn = declare_rt_send_lifecycle_extern(ctx);
    ctx.builder
        .build_call(signal_fn, &[pid.into(), variant_i64.into()], "")
        .map_err(|e| inkwell_err("build_call koja_rt_send_lifecycle", e))?;
    ctx.builder
        .build_return(None)
        .map(|_| ())
        .map_err(|e| inkwell_err("build_return signal", e))
}

/// `Ref.kill(self)` — drop the target process via `koja_rt_kill`.
fn emit_kill<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let entry_bb = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry_bb);

    let pid = pid_from_self(ctx, llvm_function, function)?;
    let kill_fn = declare_rt_kill_extern(ctx);
    ctx.builder
        .build_call(kill_fn, &[pid.into()], "")
        .map_err(|e| inkwell_err("build_call koja_rt_kill", e))?;
    ctx.builder
        .build_return(None)
        .map(|_| ())
        .map_err(|e| inkwell_err("build_return kill", e))
}

/// `Ref.alive?(self) -> Bool` — compare
/// `koja_rt_is_process_alive(pid)` against zero and return the
/// `i1` result.
fn emit_alive<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let entry_bb = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry_bb);

    let pid = pid_from_self(ctx, llvm_function, function)?;
    let alive_fn = declare_rt_is_process_alive_extern(ctx);
    let alive_i64 = ctx
        .builder
        .build_call(alive_fn, &[pid.into()], "alive_i64")
        .map_err(|e| inkwell_err("build_call koja_rt_is_process_alive", e))?
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| {
            LlvmError::Codegen("koja_rt_is_process_alive did not produce a value".to_string())
        })?
        .into_int_value();
    let zero = ctx.context.i64_type().const_int(0, false);
    let alive_bit = ctx
        .builder
        .build_int_compare(IntPredicate::NE, alive_i64, zero, "is_alive")
        .map_err(|e| inkwell_err("build_int_compare is_alive", e))?;
    ctx.builder
        .build_return(Some(&alive_bit))
        .map(|_| ())
        .map_err(|e| inkwell_err("build_return alive?", e))
}

// ----- ReplyTo method emitters --------------------------------------------

/// `ReplyTo.send(self, reply: R)` — serialize `reply` (bare `R`,
/// no `Pair` envelope) and route through `koja_rt_send` to the
/// originating caller's pid. The call-side `koja_rt_receive_timeout`
/// reader pulls `R` straight off the envelope's `+8` payload offset
/// in [`emit_call`].
fn emit_reply_send<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let entry_bb = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry_bb);

    let pid = pid_from_self(ctx, llvm_function, function)?;
    let (reply_value, reply_ir_type) = nth_param(function, llvm_function, 1)?;
    let reply_llvm = value_basic_type(ctx, reply_ir_type)?;
    let (reply_ptr, reply_len) = serialize_to_stack(ctx, "reply_msg", reply_llvm, reply_value)?;

    let send_fn = declare_rt_send_extern(ctx);
    ctx.builder
        .build_call(
            send_fn,
            &[pid.into(), reply_ptr.into(), reply_len.into()],
            "",
        )
        .map_err(|e| inkwell_err("build_call koja_rt_send (reply)", e))?;
    ctx.builder
        .build_return(None)
        .map(|_| ())
        .map_err(|e| inkwell_err("build_return reply send", e))
}

// ----- envelope construction ----------------------------------------------

/// `enum Option<T>` variant tags (declaration order in
/// `koja/lib/global/src/kernel.koja`).
const OPTION_SOME_TAG: u64 = 0;
const OPTION_NONE_TAG: u64 = 1;

/// `enum Result<T, E>` variant tag for `Ok(T)`.
const RESULT_OK_TAG: u8 = 0;
/// `enum Result<T, E>` variant tag for `Err(E)`.
const RESULT_ERR_TAG: u8 = 1;

/// `enum CallError` variant tags (declaration order in
/// `koja/lib/global/src/process.koja`).
const CALL_ERROR_TIMEOUT_TAG: u8 = 0;
const CALL_ERROR_PROCESS_DOWN_TAG: u8 = 1;

/// Synthesized LLVM type for the second field of a `Pair<M, Option
/// <ReplyTo<R>>>` envelope. `R` has no LLVM-side influence —
/// `ReplyTo<R>` always lays out as `{ i64 }`, so `Option<ReplyTo<
/// R>>` is `{ i8 tag, [7 x i8] padding, i64 reply_id }` = 16 bytes
/// regardless of `R`. We pack it into `[2 x i64]` so the writer
/// side doesn't need the receive-side's pre-emit Pair / Option
/// registry lookup; binary layout matches the receiver's typed
/// load by construction.
fn option_reply_to_payload_ty<'ctx>(ctx: &EmitContext<'ctx>) -> BasicTypeEnum<'ctx> {
    ctx.context.i64_type().array_type(2).into()
}

/// `[OPTION_NONE_TAG, 0]` — the second-field bytes for an
/// `Option::None` reply slot (`Ref.cast` / `Ref.send_after`).
fn option_none_payload<'ctx>(ctx: &EmitContext<'ctx>) -> ArrayValue<'ctx> {
    let i64_ty = ctx.context.i64_type();
    i64_ty.const_array(&[
        i64_ty.const_int(OPTION_NONE_TAG, false),
        i64_ty.const_int(0, false),
    ])
}

/// `[OPTION_SOME_TAG, reply_pid]` — the second-field bytes for an
/// `Option::Some(ReplyTo { id: reply_pid })` reply slot
/// (`Ref.call`). On little-endian hosts the tag byte sits in the
/// low byte of the first `i64`, with the trailing 7 padding bytes
/// zeroed; the reply pid occupies the second `i64`. SSA-built
/// because `reply_pid` is an `IntValue` SSA result of
/// `koja_rt_self()`.
fn option_some_payload<'ctx>(
    ctx: &EmitContext<'ctx>,
    reply_pid: IntValue<'ctx>,
) -> Result<ArrayValue<'ctx>, LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let undef = i64_ty.array_type(2).get_undef();
    let with_tag = ctx
        .builder
        .build_insert_value(
            undef,
            i64_ty.const_int(OPTION_SOME_TAG, false),
            0,
            "opt_tag",
        )
        .map_err(|e| inkwell_err("build_insert_value option some tag", e))?;
    Ok(ctx
        .builder
        .build_insert_value(with_tag, reply_pid, 1, "opt_pid")
        .map_err(|e| inkwell_err("build_insert_value option some pid", e))?
        .into_array_value())
}

/// Stack-allocate a `Pair<M, Option<ReplyTo<R>>>` envelope with
/// `msg_value` in the first field and `option_payload` in the
/// second, then return `(envelope_ptr, abi_size)` ready for an
/// `koja_rt_send` / `koja_rt_send_after` call.
fn build_pair_envelope_alloca<'ctx>(
    ctx: &EmitContext<'ctx>,
    label: &str,
    msg_ty: BasicTypeEnum<'ctx>,
    msg_value: BasicValueEnum<'ctx>,
    option_payload: ArrayValue<'ctx>,
) -> Result<(PointerValue<'ctx>, IntValue<'ctx>), LlvmError> {
    let envelope_ty: StructType<'ctx> = ctx
        .context
        .struct_type(&[msg_ty, option_reply_to_payload_ty(ctx)], false);
    let alloca = ctx.build_entry_alloca(envelope_ty, label);
    let undef = envelope_ty.get_undef();
    let with_msg = ctx
        .builder
        .build_insert_value(undef, msg_value, 0, "pair_msg")
        .map_err(|e| inkwell_err("build_insert_value pair msg", e))?
        .into_struct_value();
    let envelope = ctx
        .builder
        .build_insert_value(with_msg, option_payload, 1, "pair_option")
        .map_err(|e| inkwell_err("build_insert_value pair option", e))?
        .into_struct_value();
    ctx.builder
        .build_store(alloca, envelope)
        .map_err(|e| inkwell_err("build_store pair envelope", e))?;
    let abi_size = ctx
        .layouts
        .target_data
        .get_abi_size(&envelope_ty.as_basic_type_enum());
    let size = ctx.context.i64_type().const_int(abi_size, false);
    Ok((alloca, size))
}

/// Recover the `R` IR type from `Result<R, CallError>`'s `Ok(R)`
/// variant by walking the enum-variant payload registry. Surfaces
/// IR-seal violations as [`LlvmError::Codegen`]: `Ref.call`'s
/// return type must be a binary-shaped `Result` (typecheck enforces
/// this) and the `Ok` variant must carry exactly one positional
/// payload field of type `R`.
fn ok_payload_field_type(
    ctx: &EmitContext<'_>,
    result_symbol: &IRSymbol,
) -> Result<IRType, LlvmError> {
    let payload = ctx
        .layouts
        .enum_variant_payload(result_symbol, IRVariantTag(RESULT_OK_TAG));
    match payload {
        IRVariantPayload::Tuple(types) if types.len() == 1 => Ok(types.into_iter().next().unwrap()),
        IRVariantPayload::Struct(fields) if fields.len() == 1 => {
            Ok(fields.into_iter().next().unwrap().ir_type)
        }
        other => Err(LlvmError::Codegen(format!(
            "LLVM emit: `Ref.call` return `{result_symbol}` Ok variant has unexpected \
             payload `{other:?}` (expected single-field) — IR seal invariant violation",
        ))),
    }
}

/// Build a `Result.Err(CallError.<variant>)` SSA value. Two enum
/// constructions: the inner `CallError` variant first (no payload),
/// then the outer `Result.Err(call_error_value)`. Both go through
/// [`build_enum_value`] so layouts agree with the rest of emit.
fn build_call_error_result<'ctx>(
    ctx: &EmitContext<'ctx>,
    result_symbol: &IRSymbol,
    call_error_symbol: &IRSymbol,
    call_error_tag: u8,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let call_error_value =
        build_enum_value(ctx, call_error_symbol, IRVariantTag(call_error_tag), &[])?;
    build_enum_value(
        ctx,
        result_symbol,
        IRVariantTag(RESULT_ERR_TAG),
        &[call_error_value],
    )
}

// ----- shared helpers -----------------------------------------------------

/// Pull the i64 pid out of the `self` parameter (always param #0
/// for these methods). `Ref<M, R>` and `ReplyTo<R>` both layout as
/// `{ i64 id }`; we read field 0 directly.
fn pid_from_self<'ctx>(
    ctx: &EmitContext<'ctx>,
    llvm_function: FunctionValue<'ctx>,
    function: &IRFunction,
) -> Result<IntValue<'ctx>, LlvmError> {
    let self_value = llvm_function.get_nth_param(0).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "LLVM emit: `{}` missing self parameter",
            function.symbol,
        ))
    })?;
    let self_struct = self_value.into_struct_value();
    ctx.builder
        .build_extract_value(self_struct, 0, "pid")
        .map(|v| v.into_int_value())
        .map_err(|e| inkwell_err("build_extract_value pid", e))
}

/// Read the LLVM value + IR type for the `index`-th parameter,
/// surfacing both for downstream emission. Misses are an upstream
/// IR seal / lower bug.
fn nth_param<'ctx, 'fn_>(
    function: &'fn_ IRFunction,
    llvm_function: FunctionValue<'ctx>,
    index: u32,
) -> Result<(BasicValueEnum<'ctx>, &'fn_ IRType), LlvmError> {
    let value = llvm_function.get_nth_param(index).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "LLVM emit: `{}` missing param #{index}",
            function.symbol,
        ))
    })?;
    let ir_type = function
        .params
        .get(index as usize)
        .map(|p| &p.ty)
        .ok_or_else(|| {
            LlvmError::Codegen(format!(
                "LLVM emit: `{}` IR has no param #{index}",
                function.symbol,
            ))
        })?;
    Ok((value, ir_type))
}

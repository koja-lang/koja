//! `Ref<M, R>` and `ReplyTo<R>` `@intrinsic` emitters. Single call
//! site for the per-method `koja_rt_*` declares minted in
//! [`crate::runtime`]; the matching runtime symbols live in
//! `koja-runtime-posix/src/scheduler.rs`.
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
//!   - `Call` → mint a token via `koja_rt_call_token()`, wrap `msg`
//!     in `Pair<M, Option::Some(ReplyTo { id: caller_pid, token })>`,
//!     call `koja_rt_send`, then `koja_rt_call_receive(token,
//!     timeout)`. Three-way dispatch on the result: `0` →
//!     deserialize `R` → `Result.Ok(R)`; `-1` + target alive →
//!     `Result.Err(CallError.Timeout)`; `-1` + target dead →
//!     `Result.Err(CallError.ProcessDown)`.
//! - [`emit_reply_to`] dispatches the single [`ReplyToMethod::Send`]
//!   to a serializer + `koja_rt_reply`. The reply payload is the
//!   bare `R` value (no `Pair` envelope), correlated to the
//!   in-flight call by the `ReplyTo`'s token; the runtime routes it
//!   to the caller's one-shot reply slot.

use inkwell::AddressSpace;
use inkwell::IntPredicate;
use inkwell::types::{BasicType, BasicTypeEnum, StructType};
use inkwell::values::{ArrayValue, BasicValueEnum, FunctionValue, IntValue, PointerValue};
use koja_ir::mangling::{drop_glue_symbol, envelope_drop_glue_symbol, global_primitive_symbol};
use koja_ir::{
    IRFunction, IRSymbol, IRType, IRVariantPayload, IRVariantTag, RefMethod, ReplyToMethod,
};

use crate::ctx::EmitContext;
use crate::emit::enums::build_enum_value;
use crate::emit::heap_layout::is_heap_leaf;
use crate::emit::process::serialize_to_stack;
use crate::error::{IceExt, LlvmError};
use crate::intrinsics::element::release_in_slot;
use crate::runtime::{
    declare_rt_call_receive_extern, declare_rt_call_token_extern,
    declare_rt_is_process_alive_extern, declare_rt_kill_extern, declare_rt_reply_extern,
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
        .call_basic(self_fn, &[], "current_pid")?
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
        .or_ice()?
        .into_struct_value();
    ctx.builder
        .build_return(Some(&ref_value))
        .or_ice()
        .map(|_| ())
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
    let drop_glue = payload_drop_glue(ctx, msg_ir_type)?;

    let send_fn = declare_rt_send_extern(ctx);
    ctx.builder
        .build_call(
            send_fn,
            &[
                pid.into(),
                envelope_ptr.into(),
                envelope_size.into(),
                drop_glue.into(),
            ],
            "",
        )
        .or_ice()?;
    ctx.builder.build_return(None).or_ice().map(|_| ())
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
    let drop_glue = payload_drop_glue(ctx, msg_ir_type)?;

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
                drop_glue.into(),
            ],
            "",
        )
        .or_ice()?;
    ctx.builder.build_return(None).or_ice().map(|_| ())
}

/// `Ref.call(self, msg: M, timeout: Int) -> Result<R, CallError>`
/// — the synchronous request/reply primitive.
///
/// Mint a correlation token via `koja_rt_call_token()`, build a
/// `Pair<M, Option<ReplyTo<R>>>` envelope with the second field set
/// to `Option::Some(ReplyTo { id: caller_pid, token })`, `koja_rt_
/// send` it to the target, then block on `koja_rt_call_receive(
/// token, timeout)` — which waits on the caller's one-shot reply
/// slot, discarding stale replies from earlier timed-out calls, and
/// never touches queued business / lifecycle traffic (calls are
/// atomic). Three-way dispatch on the result:
///
/// - `0` → load `R` from the reply slot → `Result.Ok(R)`.
/// - `-1` + target alive → `Result.Err(CallError.Timeout)`.
/// - `-1` + target dead → `Result.Err(CallError.ProcessDown)`.
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
    let call_error_symbol = global_primitive_symbol(&["Process", "CallError"]);

    let self_fn = declare_rt_self_extern(ctx);
    let caller_pid = ctx.call_basic(self_fn, &[], "caller_pid")?.into_int_value();
    let token_fn = declare_rt_call_token_extern(ctx);
    let token = ctx
        .call_basic(token_fn, &[], "call_token")?
        .into_int_value();
    let some_payload = option_some_payload(ctx, caller_pid, token)?;
    let (envelope_ptr, envelope_size) =
        build_pair_envelope_alloca(ctx, "call_envelope", msg_llvm, msg_value, some_payload)?;
    let drop_glue = payload_drop_glue(ctx, msg_ir_type)?;
    let send_fn = declare_rt_send_extern(ctx);
    ctx.builder
        .build_call(
            send_fn,
            &[
                target_pid.into(),
                envelope_ptr.into(),
                envelope_size.into(),
                drop_glue.into(),
            ],
            "",
        )
        .or_ice()?;

    let reply_slot = ctx.build_entry_alloca(reply_llvm, "reply_payload");
    let reply_cap = ctx
        .context
        .i64_type()
        .const_int(ctx.layouts.target_data.get_abi_size(&reply_llvm), false);
    let receive_fn = declare_rt_call_receive_extern(ctx);
    let reply_status = ctx
        .call_basic(
            receive_fn,
            &[
                token.into(),
                reply_slot.into(),
                reply_cap.into(),
                timeout.into(),
            ],
            "reply_status",
        )?
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

    let timeout_status = ctx.context.i64_type().const_int(-1i64 as u64, true);
    let timed_out = ctx
        .builder
        .build_int_compare(
            IntPredicate::EQ,
            reply_status,
            timeout_status,
            "call_timed_out",
        )
        .or_ice()?;
    ctx.builder
        .build_conditional_branch(timed_out, timeout_check_bb, got_reply_bb)
        .or_ice()?;

    ctx.builder.position_at_end(timeout_check_bb);
    let alive_fn = declare_rt_is_process_alive_extern(ctx);
    let alive_i64 = ctx
        .call_basic(alive_fn, &[target_pid.into()], "target_alive_i64")?
        .into_int_value();
    let zero_i64 = ctx.context.i64_type().const_int(0, false);
    let target_alive = ctx
        .builder
        .build_int_compare(IntPredicate::NE, alive_i64, zero_i64, "target_alive")
        .or_ice()?;
    ctx.builder
        .build_conditional_branch(target_alive, build_timeout_bb, build_down_bb)
        .or_ice()?;

    ctx.builder.position_at_end(build_timeout_bb);
    let timeout_result = build_call_error_result(
        ctx,
        &result_symbol,
        &call_error_symbol,
        CALL_ERROR_TIMEOUT_TAG,
    )?;
    ctx.builder.build_unconditional_branch(merge_bb).or_ice()?;
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
    ctx.builder.build_unconditional_branch(merge_bb).or_ice()?;
    let down_block = ctx
        .builder
        .get_insert_block()
        .expect("EmitContext::emit_call lost the build_down insertion block before the merge phi");

    ctx.builder.position_at_end(got_reply_bb);
    let reply_value = ctx
        .builder
        .build_load(reply_llvm, reply_slot, "reply_value")
        .or_ice()?;
    let ok_result = build_enum_value(
        ctx,
        &result_symbol,
        IRVariantTag(RESULT_OK_TAG),
        &[reply_value],
    )?;
    ctx.builder.build_unconditional_branch(merge_bb).or_ice()?;
    let ok_block = ctx
        .builder
        .get_insert_block()
        .expect("EmitContext::emit_call lost the got_reply insertion block before the merge phi");

    ctx.builder.position_at_end(merge_bb);
    let result_outer = ctx.enum_outer_type(result_symbol.mangled());
    let result_phi = ctx
        .builder
        .build_phi(result_outer, "call_result")
        .or_ice()?;
    result_phi.add_incoming(&[
        (&timeout_result, timeout_block),
        (&down_result, down_block),
        (&ok_result, ok_block),
    ]);
    ctx.builder
        .build_return(Some(&result_phi.as_basic_value()))
        .or_ice()
        .map(|_| ())
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
        .or_ice()?;
    let i8_ty = ctx.context.i8_type();
    let variant_byte = ctx
        .builder
        .build_load(i8_ty, event_alloca, "variant_byte")
        .or_ice()?
        .into_int_value();
    let variant_i64 = ctx
        .builder
        .build_int_z_extend(variant_byte, ctx.context.i64_type(), "variant_i64")
        .or_ice()?;

    let signal_fn = declare_rt_send_lifecycle_extern(ctx);
    ctx.builder
        .build_call(signal_fn, &[pid.into(), variant_i64.into()], "")
        .or_ice()?;
    ctx.builder.build_return(None).or_ice().map(|_| ())
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
        .or_ice()?;
    ctx.builder.build_return(None).or_ice().map(|_| ())
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
        .call_basic(alive_fn, &[pid.into()], "alive_i64")?
        .into_int_value();
    let zero = ctx.context.i64_type().const_int(0, false);
    let alive_bit = ctx
        .builder
        .build_int_compare(IntPredicate::NE, alive_i64, zero, "is_alive")
        .or_ice()?;
    ctx.builder
        .build_return(Some(&alive_bit))
        .or_ice()
        .map(|_| ())
}

// ----- ReplyTo method emitters --------------------------------------------

/// `ReplyTo.send(self, reply: R)` — serialize `reply` (bare `R`,
/// no `Pair` envelope) and route through `koja_rt_reply` to the
/// originating caller, stamping the call's correlation token from
/// `self`. The runtime parks the envelope in the caller's one-shot
/// reply slot, where `koja_rt_call_receive` matches it by token in
/// [`emit_call`].
fn emit_reply_send<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let entry_bb = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry_bb);

    let pid = pid_from_self(ctx, llvm_function, function)?;
    let token = token_from_self(ctx, llvm_function, function)?;
    let (reply_value, reply_ir_type) = nth_param(function, llvm_function, 1)?;
    let reply_llvm = value_basic_type(ctx, reply_ir_type)?;
    let (reply_ptr, reply_len) = serialize_to_stack(ctx, "reply_msg", reply_llvm, reply_value)?;
    let drop_glue = payload_drop_glue(ctx, reply_ir_type)?;

    let delivery_symbol = match &function.return_type {
        IRType::Enum(symbol) => symbol.clone(),
        other => {
            return Err(LlvmError::Codegen(format!(
                "LLVM emit: `ReplyTo.send` returns `{other:?}` (expected the \
                 `ReplyTo.Delivery` enum) — IR seal invariant violation",
            )));
        }
    };

    let reply_fn = declare_rt_reply_extern(ctx);
    let status = ctx
        .call_basic(
            reply_fn,
            &[
                pid.into(),
                token.into(),
                reply_ptr.into(),
                reply_len.into(),
                drop_glue.into(),
            ],
            "reply_status",
        )?
        .into_int_value();

    let delivered_bb = ctx
        .context
        .append_basic_block(llvm_function, "reply_delivered");
    let expired_bb = ctx
        .context
        .append_basic_block(llvm_function, "reply_expired");
    let merge_bb = ctx.context.append_basic_block(llvm_function, "reply_merge");

    let delivered_status = ctx.context.i64_type().const_int(0, false);
    let delivered = ctx
        .builder
        .build_int_compare(IntPredicate::EQ, status, delivered_status, "reply_ok")
        .or_ice()?;
    ctx.builder
        .build_conditional_branch(delivered, delivered_bb, expired_bb)
        .or_ice()?;

    ctx.builder.position_at_end(delivered_bb);
    let delivered_value = build_delivery(ctx, &delivery_symbol, "Delivered")?;
    ctx.builder.build_unconditional_branch(merge_bb).or_ice()?;
    let delivered_block = ctx
        .builder
        .get_insert_block()
        .expect("emit_reply_send lost the delivered block before the merge phi");

    ctx.builder.position_at_end(expired_bb);
    let expired_value = build_delivery(ctx, &delivery_symbol, "Expired")?;
    ctx.builder.build_unconditional_branch(merge_bb).or_ice()?;
    let expired_block = ctx
        .builder
        .get_insert_block()
        .expect("emit_reply_send lost the expired block before the merge phi");

    ctx.builder.position_at_end(merge_bb);
    let outer = ctx.enum_outer_type(delivery_symbol.mangled());
    let phi = ctx.builder.build_phi(outer, "reply_delivery").or_ice()?;
    phi.add_incoming(&[
        (&delivered_value, delivered_block),
        (&expired_value, expired_block),
    ]);
    ctx.builder
        .build_return(Some(&phi.as_basic_value()))
        .or_ice()
        .map(|_| ())
}

/// Build a nullary `ReplyTo.Delivery` variant (`Delivered` / `Expired`),
/// resolving its tag by name so the enum's declaration order isn't baked
/// into codegen.
fn build_delivery<'ctx>(
    ctx: &EmitContext<'ctx>,
    delivery_symbol: &IRSymbol,
    variant: &str,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let tag = ctx.layouts.enum_variant_tag(delivery_symbol, variant);
    build_enum_value(ctx, delivery_symbol, tag, &[])
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
/// `ReplyTo<R>` always lays out as `{ i64 id, i64 token }`, so
/// `Option<ReplyTo<R>>` is `{ i8 tag, [7 x i8] padding, i64
/// reply_id, i64 token }` = 24 bytes regardless of `R`. We pack it
/// into `[3 x i64]` so the writer side doesn't need the
/// receive-side's pre-emit Pair / Option registry lookup; binary
/// layout matches the receiver's typed load by construction.
fn option_reply_to_payload_ty<'ctx>(ctx: &EmitContext<'ctx>) -> BasicTypeEnum<'ctx> {
    ctx.context.i64_type().array_type(3).into()
}

/// `[OPTION_NONE_TAG, 0, 0]` — the second-field bytes for an
/// `Option::None` reply slot (`Ref.cast` / `Ref.send_after`).
fn option_none_payload<'ctx>(ctx: &EmitContext<'ctx>) -> ArrayValue<'ctx> {
    let i64_ty = ctx.context.i64_type();
    i64_ty.const_array(&[
        i64_ty.const_int(OPTION_NONE_TAG, false),
        i64_ty.const_int(0, false),
        i64_ty.const_int(0, false),
    ])
}

/// `[OPTION_SOME_TAG, reply_pid, token]` — the second-field bytes
/// for an `Option::Some(ReplyTo { id: reply_pid, token })` reply
/// slot (`Ref.call`). On little-endian hosts the tag byte sits in
/// the low byte of the first `i64`, with the trailing 7 padding
/// bytes zeroed; the reply pid and correlation token occupy the
/// next two. SSA-built because both are SSA results
/// (`koja_rt_self()` / `koja_rt_call_token()`).
fn option_some_payload<'ctx>(
    ctx: &EmitContext<'ctx>,
    reply_pid: IntValue<'ctx>,
    token: IntValue<'ctx>,
) -> Result<ArrayValue<'ctx>, LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let undef = i64_ty.array_type(3).get_undef();
    let with_tag = ctx
        .builder
        .build_insert_value(
            undef,
            i64_ty.const_int(OPTION_SOME_TAG, false),
            0,
            "opt_tag",
        )
        .or_ice()?;
    let with_pid = ctx
        .builder
        .build_insert_value(with_tag, reply_pid, 1, "opt_pid")
        .or_ice()?;
    Ok(ctx
        .builder
        .build_insert_value(with_pid, token, 2, "opt_token")
        .or_ice()?
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
        .or_ice()?
        .into_struct_value();
    let envelope = ctx
        .builder
        .build_insert_value(with_msg, option_payload, 1, "pair_option")
        .or_ice()?
        .into_struct_value();
    ctx.builder.build_store(alloca, envelope).or_ice()?;
    let abi_size = ctx
        .layouts
        .target_data
        .get_abi_size(&envelope_ty.as_basic_type_enum());
    let size = ctx.context.i64_type().const_int(abi_size, false);
    Ok((alloca, size))
}

/// Build (or look up) the by-pointer payload drop shim for `payload`
/// and return its address as the `void(i8*)*` value the `koja_rt_send`
/// / `koja_rt_send_after` / `koja_rt_reply` / `koja_rt_spawn`
/// `drop_glue` argument expects. Returns a null pointer when the
/// payload owns no nested Koja heap (scalars, no-glue aggregates) —
/// the runtime then frees only its own buffer on discard.
///
/// The runtime's discard path is type-erased (`fn(*mut u8)` over the
/// payload bytes), an ABI the by-value `drop_T` can't satisfy. The
/// shim bridges the two: it loads `payload` through the pointer and
/// routes into `drop_T` via [`release_in_slot`]. Content-addressed by
/// [`envelope_drop_glue_symbol`], so every send site for the same
/// message / reply / config type shares one shim.
pub(crate) fn payload_drop_glue<'ctx>(
    ctx: &EmitContext<'ctx>,
    payload: &IRType,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    if !payload_owns_heap(ctx, payload) {
        return Ok(ptr_ty.const_null().into());
    }
    let symbol = envelope_drop_glue_symbol(payload);
    let shim = match ctx.module.get_function(symbol.mangled()) {
        Some(existing) => existing,
        None => build_payload_drop_shim(ctx, payload, symbol.mangled())?,
    };
    Ok(shim.as_global_value().as_pointer_value().into())
}

/// Whether `payload` carries any nested Koja heap to release on
/// discard: a heap leaf (`String` / `Binary` / `Bits`) or a composite
/// with declared `drop_T`. Scalars and no-glue aggregates answer
/// `false`, so [`payload_drop_glue`] hands the runtime a null glue.
fn payload_owns_heap(ctx: &EmitContext<'_>, payload: &IRType) -> bool {
    is_heap_leaf(payload) || ctx.declared_function(&drop_glue_symbol(payload)).is_some()
}

/// Synthesize the `void(i8*)` envelope-drop shim body: load the
/// payload through its pointer and release it via [`release_in_slot`].
/// Saves and restores the builder position so it can be minted in the
/// middle of a send emitter's body.
fn build_payload_drop_shim<'ctx>(
    ctx: &EmitContext<'ctx>,
    payload: &IRType,
    symbol: &str,
) -> Result<FunctionValue<'ctx>, LlvmError> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let signature = ctx.context.void_type().fn_type(&[ptr_ty.into()], false);
    let shim = ctx.module.add_function(symbol, signature, None);
    let saved = ctx.builder.get_insert_block();
    let entry = ctx.context.append_basic_block(shim, "entry");
    ctx.builder.position_at_end(entry);
    let payload_ptr = shim
        .get_nth_param(0)
        .ok_or_else(|| {
            LlvmError::Codegen(format!(
                "envelope drop shim `{symbol}` missing payload param"
            ))
        })?
        .into_pointer_value();
    release_in_slot(ctx, payload, payload_ptr)?;
    ctx.builder.build_return(None).or_ice()?;
    if let Some(saved) = saved {
        ctx.builder.position_at_end(saved);
    }
    Ok(shim)
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
/// for these methods). `Ref<M, R>` lays out as `{ i64 id }` and
/// `ReplyTo<R>` as `{ i64 id, i64 token }`; the pid is field 0 of
/// both.
fn pid_from_self<'ctx>(
    ctx: &EmitContext<'ctx>,
    llvm_function: FunctionValue<'ctx>,
    function: &IRFunction,
) -> Result<IntValue<'ctx>, LlvmError> {
    self_field(ctx, llvm_function, function, 0, "pid")
}

/// Pull the i64 correlation token out of a `ReplyTo<R>` `self`
/// parameter (field 1; see [`pid_from_self`] for the layout).
fn token_from_self<'ctx>(
    ctx: &EmitContext<'ctx>,
    llvm_function: FunctionValue<'ctx>,
    function: &IRFunction,
) -> Result<IntValue<'ctx>, LlvmError> {
    self_field(ctx, llvm_function, function, 1, "token")
}

fn self_field<'ctx>(
    ctx: &EmitContext<'ctx>,
    llvm_function: FunctionValue<'ctx>,
    function: &IRFunction,
    index: u32,
    name: &str,
) -> Result<IntValue<'ctx>, LlvmError> {
    let self_value = llvm_function.get_nth_param(0).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "LLVM emit: `{}` missing self parameter",
            function.symbol,
        ))
    })?;
    let self_struct = self_value.into_struct_value();
    ctx.builder
        .build_extract_value(self_struct, index, name)
        .or_ice()
        .map(|v| v.into_int_value())
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

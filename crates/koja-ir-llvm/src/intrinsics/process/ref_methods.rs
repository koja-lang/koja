//! `Ref<M, R>` `@intrinsic` emitters. The message-bearing methods
//! share the `(M, Option<ReplyTo<R>>)` envelope the receive side
//! reads, with the reply slot `None` for `cast` and `send_after` and
//! `Some(ReplyTo { id, token })` for `call`. Reply payloads travel
//! bare, correlated to the in-flight call by the `ReplyTo`'s token.

use inkwell::IntPredicate;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue};
use koja_ir::mangling::global_primitive_symbol;
use koja_ir::{IRFunction, IRSymbol, IRType, IRVariantPayload};

use crate::ctx::EmitContext;
use crate::emit::enums::build_enum_value;
use crate::error::{IceExt, LlvmError};
use crate::intrinsics::result;
use crate::intrinsics::util::{nth_int, nth_param, nth_param_type};
use crate::runtime::{
    declare_rt_call_receive_extern, declare_rt_call_token_extern,
    declare_rt_is_process_alive_extern, declare_rt_kill_extern, declare_rt_self_extern,
    declare_rt_send_after_extern, declare_rt_send_extern, declare_rt_send_lifecycle_extern,
};
use crate::types::ir_basic_type;

use super::envelope::{
    build_tuple_envelope_alloca, option_none_payload, option_some_payload, payload_drop_glue,
};
use super::pid_from_self;

/// `Ref.alive?(self) -> Bool`. Compare `koja_rt_is_process_alive(pid)`
/// against zero and return the `i1` result.
pub(super) fn emit_alive<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
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

/// `Ref.call(self, msg: M, timeout: Int) -> Result<R, CallError>`.
/// Mint a token with `koja_rt_call_token`, send a
/// `(M, Some(ReplyTo { id: caller_pid, token }))` envelope, then
/// block on `koja_rt_call_receive`. Its result maps as:
///
/// - `0` -> `Result.Ok(R)` loaded from the reply slot.
/// - `-1` + target alive -> `Result.Err(CallError.Timeout)`.
/// - `-1` + target dead -> `Result.Err(CallError.ProcessDown)`.
pub(super) fn emit_call<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let target_pid = pid_from_self(ctx, llvm_function, function)?;
    let msg_value = nth_param(function, llvm_function, 1, "msg");
    let msg_ir_type = nth_param_type(function, 1);
    let timeout = nth_int(function, llvm_function, 2, "timeout");
    let msg_llvm = ir_basic_type(ctx, msg_ir_type)?;

    let result_symbol = match &function.return_type {
        IRType::Enum(symbol) => symbol.clone(),
        other => panic!(
            "LLVM emit: `Ref.call` returns `{other:?}`, expected Enum \
             (IR seal invariant violation)",
        ),
    };
    let reply_ir_type = ok_payload_field_type(ctx, &result_symbol);
    let reply_llvm = ir_basic_type(ctx, &reply_ir_type)?;
    let call_error_symbol = global_primitive_symbol(&["Process", "CallError"]);

    let self_fn = declare_rt_self_extern(ctx);
    let caller_pid = ctx.call_basic(self_fn, &[], "caller_pid")?.into_int_value();
    let token_fn = declare_rt_call_token_extern(ctx);
    let token = ctx
        .call_basic(token_fn, &[], "call_token")?
        .into_int_value();
    let some_payload = option_some_payload(ctx, caller_pid, token)?;
    let (envelope_ptr, envelope_size) =
        build_tuple_envelope_alloca(ctx, "call_envelope", msg_llvm, msg_value, some_payload)?;
    let drop_glue = payload_drop_glue(ctx, msg_ir_type)?;
    ctx.call_rt_unit(
        declare_rt_send_extern,
        &[
            target_pid.into(),
            envelope_ptr.into(),
            envelope_size.into(),
            drop_glue.into(),
        ],
    )?;

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
                target_pid.into(),
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
    let timeout_result =
        build_call_error_result(ctx, &result_symbol, &call_error_symbol, "Timeout")?;
    ctx.builder.build_unconditional_branch(merge_bb).or_ice()?;
    let timeout_block = ctx.builder.get_insert_block().expect(
        "EmitContext::emit_call lost the build_timeout insertion block before the merge phi",
    );

    ctx.builder.position_at_end(build_down_bb);
    let down_result =
        build_call_error_result(ctx, &result_symbol, &call_error_symbol, "ProcessDown")?;
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
        result::ok_tag(ctx, &result_symbol),
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

/// `Ref.kill(self)`. Drop the target process via `koja_rt_kill`.
pub(super) fn emit_kill<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let pid = pid_from_self(ctx, llvm_function, function)?;
    ctx.call_rt_unit(declare_rt_kill_extern, &[pid.into()])?;
    ctx.builder.build_return(None).or_ice().map(|_| ())
}

/// `Ref.self_ref() -> Ref<M, R>`. Call `koja_rt_self()` and wrap
/// the returned pid in the `Ref` struct value the function's return
/// type already specifies.
pub(super) fn emit_self_ref<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
) -> Result<(), LlvmError> {
    let self_fn = declare_rt_self_extern(ctx);
    let pid = ctx
        .call_basic(self_fn, &[], "current_pid")?
        .into_int_value();

    let ref_struct = match &function.return_type {
        IRType::Struct(symbol) => ctx.layouts.struct_type(symbol.mangled()),
        other => panic!(
            "LLVM emit: `Ref.self_ref` returns `{other:?}`, expected Struct \
             (IR seal invariant violation)",
        ),
    };
    let ref_value = ctx
        .builder
        .build_insert_value(ref_struct.get_undef(), pid, 0, "ref_pid")
        .or_ice()?
        .into_struct_value();
    ctx.builder
        .build_return(Some(&ref_value))
        .or_ice()
        .map(|_| ())
}

/// `Ref.cast(self, msg: M)` and `Ref.send_after(self, msg: M,
/// delay_ms: Int)`. Both wrap `msg` in a `(M, Option<ReplyTo<R>>)`
/// envelope with the reply slot set to `Option::None`. Without a
/// `delay` the envelope goes through `koja_rt_send`. With one it
/// goes through `koja_rt_send_after` and the trailing delay
/// parameter.
pub(super) fn emit_send_envelope<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    delay: Option<IntValue<'ctx>>,
) -> Result<(), LlvmError> {
    let pid = pid_from_self(ctx, llvm_function, function)?;
    let msg_value = nth_param(function, llvm_function, 1, "msg");
    let msg_ir_type = nth_param_type(function, 1);
    let msg_llvm = ir_basic_type(ctx, msg_ir_type)?;
    let label = if delay.is_some() {
        "send_after_envelope"
    } else {
        "cast_envelope"
    };
    let (envelope_ptr, envelope_size) =
        build_tuple_envelope_alloca(ctx, label, msg_llvm, msg_value, option_none_payload(ctx))?;
    let drop_glue = payload_drop_glue(ctx, msg_ir_type)?;

    match delay {
        Some(delay) => ctx.call_rt_unit(
            declare_rt_send_after_extern,
            &[
                pid.into(),
                envelope_ptr.into(),
                envelope_size.into(),
                delay.into(),
                drop_glue.into(),
            ],
        )?,
        None => ctx.call_rt_unit(
            declare_rt_send_extern,
            &[
                pid.into(),
                envelope_ptr.into(),
                envelope_size.into(),
                drop_glue.into(),
            ],
        )?,
    }
    ctx.builder.build_return(None).or_ice().map(|_| ())
}

/// `Ref.signal(self, event: Lifecycle)`. Pull the lifecycle variant
/// byte (offset 0 of the enum's outer struct) and call
/// `koja_rt_send_lifecycle(pid, variant)`. The runtime maps variant
/// indices `0=Shutdown, 1=Interrupt, 2=Reload`, matching the AST
/// declaration order in `Global.process`.
pub(super) fn emit_signal<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let pid = pid_from_self(ctx, llvm_function, function)?;
    let event_value = nth_param(function, llvm_function, 1, "event");
    let event_ir_type = nth_param_type(function, 1);
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
    ctx.call_rt_unit(
        declare_rt_send_lifecycle_extern,
        &[pid.into(), variant_i64.into()],
    )?;
    ctx.builder.build_return(None).or_ice().map(|_| ())
}

/// Build a `Result.Err(CallError.<variant>)` SSA value in two enum
/// constructions, the inner `CallError` variant first (no payload),
/// then the outer `Result.Err(call_error_value)`. Both go through
/// [`build_enum_value`] with name-resolved tags so layouts agree
/// with the rest of emit.
fn build_call_error_result<'ctx>(
    ctx: &EmitContext<'ctx>,
    result_symbol: &IRSymbol,
    call_error_symbol: &IRSymbol,
    variant: &str,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let tag = ctx.layouts.enum_variant_tag(call_error_symbol, variant);
    let call_error_value = build_enum_value(ctx, call_error_symbol, tag, &[])?;
    build_enum_value(
        ctx,
        result_symbol,
        result::err_tag(ctx, result_symbol),
        &[call_error_value],
    )
}

/// Recover the `R` IR type from `Result<R, CallError>`'s `Ok(R)`
/// variant by walking the enum-variant payload registry. Panics on
/// an IR-seal violation. `Ref.call`'s return type must be a
/// binary-shaped `Result` (typecheck enforces this) and the `Ok`
/// variant must carry exactly one positional payload field of type
/// `R`.
fn ok_payload_field_type(ctx: &EmitContext<'_>, result_symbol: &IRSymbol) -> IRType {
    let payload = ctx
        .layouts
        .enum_variant_payload(result_symbol, result::ok_tag(ctx, result_symbol));
    match payload {
        IRVariantPayload::Tuple(types) if types.len() == 1 => types.into_iter().next().unwrap(),
        IRVariantPayload::Struct(fields) if fields.len() == 1 => {
            fields.into_iter().next().unwrap().ir_type
        }
        other => panic!(
            "LLVM emit: `Ref.call` return `{result_symbol}` Ok variant has unexpected \
             payload `{other:?}`, expected single-field (IR seal invariant violation)",
        ),
    }
}
